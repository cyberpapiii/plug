use super::*;
use crate::legacy_tasks::{
    CancelTaskParams as LegacyCancelTaskParams, GetTaskParams as LegacyGetTaskParams,
    GetTaskPayloadParams as LegacyGetTaskPayloadParams,
};

struct StdioDownstreamContext {
    client_id: Arc<str>,
    request_id: RequestId,
    client_type: ClientType,
    protocol_version: ProtocolVersion,
    client_metadata: Option<crate::protocol::ClientMetadata>,
    modern_direction_enabled: bool,
    cancellation: CancellationToken,
}

impl crate::dispatch::DownstreamContext for StdioDownstreamContext {
    fn downstream_call_context(&self) -> DownstreamCallContext {
        let mut context = DownstreamCallContext::stdio_for_client(
            Arc::clone(&self.client_id),
            self.request_id.clone(),
            self.client_type,
        )
        .with_protocol(
            crate::protocol::ProtocolEra::from_version(&self.protocol_version),
            self.protocol_version.to_string(),
        )
        .with_modern_direction_enabled(self.modern_direction_enabled)
        .with_lifecycle(None, self.cancellation.clone(), None);
        if let Some(metadata) = &self.client_metadata {
            context = context
                .with_client_metadata(Arc::clone(&metadata.name), Arc::clone(&metadata.version));
        }
        context
    }

    /// stdio's `tools/call` handler can only return a `CallToolResult`, so its
    /// handler rejects legacy task-augmented calls before shared dispatch.
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
        request: ElicitRequestParams,
    ) -> Pin<Box<dyn Future<Output = Result<ElicitResult, McpError>> + Send + '_>> {
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

    /// Apply the reloadable modern-downstream gate for this handler.
    pub fn set_modern_downstream_enabled(&self, enabled: bool) {
        self.router.set_modern_downstream_enabled(enabled);
    }

    pub fn modern_downstream_enabled(&self) -> bool {
        self.router.modern_downstream_enabled()
    }

    fn downstream_context_for_call(
        &self,
        request_id: RequestId,
        protocol_version: ProtocolVersion,
        client_info: Option<Implementation>,
        cancellation: CancellationToken,
    ) -> StdioDownstreamContext {
        let client_type = self
            .client_type
            .read()
            .map(|ct| *ct)
            .unwrap_or(ClientType::Unknown);
        StdioDownstreamContext {
            client_id: Arc::clone(&self.client_id),
            request_id,
            client_type,
            protocol_version,
            client_metadata: client_info.map(|info| crate::protocol::ClientMetadata {
                name: Arc::from(info.name),
                version: Arc::from(info.version),
            }),
            modern_direction_enabled: self.router.modern_downstream_enabled(),
            cancellation,
        }
    }

    #[cfg(test)]
    pub(crate) fn client_id(&self) -> Arc<str> {
        Arc::clone(&self.client_id)
    }
}

#[allow(clippy::manual_async_fn)]
impl ServerHandler for ProxyHandler {
    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        let mut versions = vec![crate::protocol::supported_protocol_version()];
        if self.modern_downstream_enabled() {
            versions.push(ProtocolVersion::V_2026_07_28);
        }
        std::borrow::Cow::Owned(versions)
    }

    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(self.router.synthesized_capabilities())
            .with_server_info(plug_implementation())
            .with_protocol_version(crate::protocol::supported_protocol_version())
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.router.get_tool_definition(name)
    }

    fn discover(
        &self,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<DiscoverResult, McpError>> + Send + '_ {
        async move {
            if !self.modern_downstream_enabled() {
                return Err(McpError::unsupported_protocol_version(
                    ProtocolVersion::V_2026_07_28,
                    &self.supported_protocol_versions(),
                ));
            }
            let client_info = context.client_info().ok_or_else(|| {
                McpError::invalid_params("discover requires client implementation metadata", None)
            })?;
            let capabilities = context.client_capabilities().ok_or_else(|| {
                McpError::invalid_params("discover requires client capability metadata", None)
            })?;
            let initialize = InitializeRequestParams::new(capabilities, client_info)
                .with_protocol_version(ProtocolVersion::V_2026_07_28);

            // Reuse the established connection-state setup without requiring a
            // legacy initialize message on the wire. This records the client,
            // peer, capability and notification state identically for both
            // lifecycles.
            let mut info = self.initialize(initialize, context).await?;
            // U4 exposes only the ordinary core. Tasks, Apps and every other
            // extension stay unadvertised until their bridges have their own
            // conformance evidence.
            crate::protocol::suppress_unimplemented_modern_capabilities(&mut info.capabilities);
            Ok(DiscoverResult::from_server_info(
                self.supported_protocol_versions().into_owned(),
                info,
            ))
        }
    }

    /*fn enqueue_task(
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
                    // stdio has no teardown path that calls task cleanup, so
                    // the owner is always live — nothing to probe.
                    None,
                    Some(downstream),
                )
                .await
        }
    }*/

    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<InitializeResult, McpError>> + Send + '_ {
        async move {
            if request.protocol_version == ProtocolVersion::V_2026_07_28 {
                if !self.modern_downstream_enabled() {
                    return Err(McpError::unsupported_protocol_version(
                        request.protocol_version,
                        &self.supported_protocol_versions(),
                    ));
                }
            } else {
                crate::protocol::ensure_supported_downstream_protocol(&request.protocol_version)?;
            }

            let selected_protocol = request.protocol_version.clone();
            let client_type = detect_client(&request.client_info.name);
            tracing::info!(
                client = %request.client_info.name,
                detected = %client_type,
                requested_protocol = %request.protocol_version,
                selected_protocol = crate::protocol::SUPPORTED_PROTOCOL_VERSION,
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
                // This connection's own identity, for fanout::resolve()'s
                // target match — see plug-core/src/notifications.rs::fanout.
                let identity = NotificationTarget::Stdio {
                    client_id: Arc::clone(&client_id),
                };
                tokio::spawn(async move {
                    loop {
                        let msg = tokio::select! {
                            biased;
                            _ = shutdown.cancelled() => break,
                            msg = rx.recv() => msg,
                        };
                        match msg {
                            Ok(notification) => {
                                // classify -> resolve -> (per-notification-kind delivery below).
                                // deliver_to() collapses the four `matches!(target,
                                // NotificationTarget::Stdio {..} if ..)` checks this block used
                                // to repeat into one shared comparison; it's a no-op (always
                                // true) for broadcast-shaped notifications.
                                let deliver = crate::notifications::fanout::resolve(
                                    crate::notifications::fanout::classify(&notification),
                                )
                                .deliver_to(&identity);
                                match notification {
                                    ProtocolNotification::ToolListChanged => {
                                        if let Err(error) = peer.notify_tool_list_changed().await {
                                            tracing::debug!(
                                                error = %error,
                                                "stopping stdio notification fan-out after peer send failure"
                                            );
                                            break;
                                        }
                                    }
                                    ProtocolNotification::ToolListChangedFor { .. } => {
                                        if deliver && peer.notify_tool_list_changed().await.is_err()
                                        {
                                            break;
                                        }
                                    }
                                    ProtocolNotification::ResourceListChanged => {
                                        if let Err(error) = peer.notify_resource_list_changed().await
                                        {
                                            tracing::debug!(
                                                error = %error,
                                                "stopping stdio notification fan-out after peer send failure"
                                            );
                                            break;
                                        }
                                    }
                                    ProtocolNotification::PromptListChanged => {
                                        if let Err(error) = peer.notify_prompt_list_changed().await {
                                            tracing::debug!(
                                                error = %error,
                                                "stopping stdio notification fan-out after peer send failure"
                                            );
                                            break;
                                        }
                                    }
                                    ProtocolNotification::Progress { params, .. } => {
                                        if deliver && peer.notify_progress(params).await.is_err() {
                                            break;
                                        }
                                    }
                                    ProtocolNotification::Cancelled { params, .. } => {
                                        if deliver && peer.notify_cancelled(params).await.is_err() {
                                            break;
                                        }
                                    }
                                    ProtocolNotification::ResourceUpdated { params, .. } => {
                                        if deliver
                                            && peer.notify_resource_updated(params).await.is_err()
                                        {
                                            break;
                                        }
                                    }
                                    ref notification @ (ProtocolNotification::LoggingMessage {
                                        ..
                                    }
                                    | ProtocolNotification::TokenRefreshExchanged { .. }
                                    | ProtocolNotification::AuthStateChanged { .. }) => {
                                        if let Some(params) =
                                            notification.as_logging_message_params()
                                            && peer.notify_logging_message(params).await.is_err()
                                        {
                                            break;
                                        }
                                    }
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
                                    .notify_logging_message(
                                        LoggingMessageNotificationParam::new(
                                            LoggingLevel::Warning,
                                            serde_json::json!(format!(
                                                "skipped {skipped} log messages"
                                            )),
                                        )
                                        .with_logger("plug"),
                                    )
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
                    .with_protocol_version(selected_protocol),
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

    /*fn list_tasks(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListTasksResult, McpError>> + Send + '_ {
        let router = Arc::clone(&self.router);
        let owner = TaskOwner::new(Arc::<str>::from(format!("stdio:{}", self.client_id)));
        async move { router.list_tasks_for_owner(&owner, request).await }
    }*/

    fn call_tool(
        &self,
        mut request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, McpError>> + Send + '_ {
        async move {
            // RMCP 3.x removes wire-level `params._meta` from typed params and
            // exposes it through RequestContext instead. Rehydrate ordinary
            // request metadata before dispatch, and preserve the legacy stdio
            // contract that a task wrapper on a non-task-capable surface is an
            // INVALID_PARAMS error rather than silently executing synchronously.
            if !context.meta.is_empty() {
                match request.meta.as_mut() {
                    Some(meta) => meta.extend(context.meta.clone()),
                    None => request.meta = Some(context.meta.clone()),
                }
            }
            if request
                .meta
                .as_ref()
                .is_some_and(|meta| meta.contains_key(crate::protocol::LEGACY_TASK_REQUEST_KEY))
            {
                return Err(McpError::invalid_params(
                    "stdio does not support task-augmented tools/call".to_string(),
                    None,
                ));
            }

            let protocol_version = context
                .protocol_version()
                .unwrap_or_else(crate::protocol::supported_protocol_version);
            let ctx = self.downstream_context_for_call(
                context.id.clone(),
                protocol_version,
                context.client_info(),
                context.ct.clone(),
            );
            match crate::dispatch::dispatch_tools_call(&self.router, &ctx, request).await? {
                crate::dispatch::ToolCallOutcome::Called(result) => Ok(result.into()),
                // `supports_tasks()` is false for stdio, so a task outcome is
                // unreachable after the explicit legacy task rejection above.
                crate::dispatch::ToolCallOutcome::TaskCreated(_) => Err(McpError::internal_error(
                    "stdio tools/call unexpectedly produced a task result".to_string(),
                    None,
                )),
            }
        }
    }

    /*fn get_task_info(
        &self,
        request: GetTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetTaskResult, McpError>> + Send + '_ {
        let router = Arc::clone(&self.router);
        let owner = TaskOwner::new(Arc::<str>::from(format!("stdio:{}", self.client_id)));
        async move {
            router
                .get_task_info_for_owner(&owner, &request.task_id)
                .await
        }
    }*/

    /*fn get_task_result(
        &self,
        request: GetTaskPayloadParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetTaskPayloadResult, McpError>> + Send + '_ {
        let router = Arc::clone(&self.router);
        let owner = TaskOwner::new(Arc::<str>::from(format!("stdio:{}", self.client_id)));
        async move {
            router
                .get_task_result_for_owner(&owner, &request.task_id)
                .await
        }
    }*/

    /*fn cancel_task(
        &self,
        request: CancelTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CancelTaskResult, McpError>> + Send + '_ {
        let router = Arc::clone(&self.router);
        let owner = TaskOwner::new(Arc::<str>::from(format!("stdio:{}", self.client_id)));
        async move { router.cancel_task_for_owner(&owner, &request.task_id).await }
    }*/

    fn on_custom_request(
        &self,
        request: CustomRequest,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CustomResult, McpError>> + Send + '_ {
        let router = Arc::clone(&self.router);
        let owner = TaskOwner::new(Arc::<str>::from(format!("stdio:{}", self.client_id)));
        async move {
            let value = match request.method.as_str() {
                "plug/legacy/tasks/list" => {
                    let params = request
                        .params_as::<PaginatedRequestParams>()
                        .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
                    serde_json::to_value(router.list_tasks_for_owner(&owner, params).await?)
                }
                "plug/legacy/tasks/get" => {
                    let params = request
                        .params_as::<LegacyGetTaskParams>()
                        .map_err(|e| McpError::invalid_params(e.to_string(), None))?
                        .ok_or_else(|| McpError::invalid_params("missing task params", None))?;
                    serde_json::to_value(
                        router
                            .get_task_info_for_owner(&owner, &params.task_id)
                            .await?,
                    )
                }
                "plug/legacy/tasks/result" => {
                    let params = request
                        .params_as::<LegacyGetTaskPayloadParams>()
                        .map_err(|e| McpError::invalid_params(e.to_string(), None))?
                        .ok_or_else(|| McpError::invalid_params("missing task params", None))?;
                    serde_json::to_value(
                        router
                            .get_task_result_for_owner(&owner, &params.task_id)
                            .await?,
                    )
                }
                "plug/legacy/tasks/cancel" => {
                    let params = request
                        .params_as::<LegacyCancelTaskParams>()
                        .map_err(|e| McpError::invalid_params(e.to_string(), None))?
                        .ok_or_else(|| McpError::invalid_params("missing task params", None))?;
                    serde_json::to_value(
                        router
                            .cancel_task_for_owner(&owner, &params.task_id)
                            .await?,
                    )
                }
                _ => {
                    return Err(McpError::new(
                        ErrorCode::METHOD_NOT_FOUND,
                        request.method,
                        None,
                    ));
                }
            }
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            Ok(CustomResult::new(value))
        }
    }

    fn on_cancelled(
        &self,
        notification: CancelledNotificationParam,
        _context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + Send + '_ {
        async move {
            if let Some(request_id) = notification.request_id {
                self.router.forward_cancel_from_downstream(
                    &DownstreamCallContext::stdio(Arc::clone(&self.client_id), request_id),
                    notification.reason,
                );
            }
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
    ) -> impl Future<Output = Result<ReadResourceResponse, McpError>> + Send + '_ {
        async move {
            self.router
                .read_resource(&request.uri)
                .await
                .map(Into::into)
        }
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
    ) -> impl Future<Output = Result<GetPromptResponse, McpError>> + Send + '_ {
        async move {
            self.router
                .get_prompt(&request.name, request.arguments)
                .await
                .map(Into::into)
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

#[cfg(test)]
mod modern_context_tests {
    use super::*;
    use crate::dispatch::DownstreamContext;

    #[test]
    fn discovered_modern_call_fails_closed_after_live_gate_is_disabled() {
        let handler = ProxyHandler::new(
            Arc::new(ServerManager::new()),
            RouterConfig::from(&crate::config::Config::default()),
        );
        handler.set_modern_downstream_enabled(true);
        handler.set_modern_downstream_enabled(false);

        let context = handler
            .downstream_context_for_call(
                RequestId::Number(7),
                ProtocolVersion::V_2026_07_28,
                Some(Implementation::new("modern-client", "1.0")),
                CancellationToken::new(),
            )
            .downstream_call_context();

        assert_eq!(context.protocol_era, crate::protocol::ProtocolEra::Modern);
        assert_eq!(
            context
                .client_metadata
                .as_ref()
                .map(|meta| meta.name.as_ref()),
            Some("modern-client")
        );
        assert_eq!(
            context.authorize(crate::protocol::MethodFamily::ToolsCall),
            Err(crate::protocol::ProtocolOutcome::UnsupportedBridge
                .into_error(crate::protocol::ProtocolEra::Modern))
        );
    }
}
