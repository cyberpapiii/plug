use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use futures::{StreamExt, stream::BoxStream};
use http::{HeaderName, header};
use reqwest::StatusCode;
use rmcp::RoleClient;
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::common::client_side_sse::{ExponentialBackoff, SseRetryPolicy};
use rmcp::transport::worker::{Worker, WorkerConfig, WorkerContext, WorkerQuitReason};
use rmcp::transport::{Transport, WorkerTransport};
use sse_stream::SseStream;

type BoxedSseStream = BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LegacySseError {
    #[error("reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("sse parse error: {0}")]
    Sse(#[from] sse_stream::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("stream closed before endpoint event")]
    MissingEndpoint,
    #[error("invalid endpoint event payload: {0}")]
    InvalidEndpoint(String),
    #[error("unexpected SSE response status {status}: {body}")]
    UnexpectedStatus { status: StatusCode, body: String },
    #[error("unexpected SSE content type: {0:?}")]
    UnexpectedContentType(Option<String>),
    #[error("transport channel closed")]
    TransportChannelClosed,
    #[error("join error: {0}")]
    Join(#[from] tokio::task::JoinError),
}

impl LegacySseError {
    pub fn is_legacy_fallback_hint(&self) -> bool {
        matches!(
            self,
            Self::UnexpectedStatus { status, .. }
                if matches!(*status, StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED)
        )
    }
}

#[derive(Clone)]
pub struct LegacySseTransportConfig {
    pub uri: Arc<str>,
    pub auth_token: Option<Arc<str>>,
    pub channel_buffer_capacity: usize,
    pub endpoint_wait_timeout: Duration,
    pub retry_policy: Arc<dyn SseRetryPolicy>,
}

impl std::fmt::Debug for LegacySseTransportConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LegacySseTransportConfig")
            .field("uri", &self.uri)
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("channel_buffer_capacity", &self.channel_buffer_capacity)
            .field("endpoint_wait_timeout", &self.endpoint_wait_timeout)
            .finish()
    }
}

impl LegacySseTransportConfig {
    pub fn with_uri(uri: impl Into<Arc<str>>) -> Self {
        Self {
            uri: uri.into(),
            auth_token: None,
            channel_buffer_capacity: 16,
            endpoint_wait_timeout: Duration::from_secs(5),
            retry_policy: Arc::new(ExponentialBackoff {
                max_times: None,
                base_duration: Duration::from_millis(1_000),
            }),
        }
    }

    pub fn auth_token(mut self, token: impl Into<Arc<str>>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    pub fn endpoint_wait_timeout(mut self, timeout: Duration) -> Self {
        self.endpoint_wait_timeout = timeout;
        self
    }
}

pub struct LegacySseClientTransport(WorkerTransport<LegacySseWorker>);

impl LegacySseClientTransport {
    pub fn from_config(config: LegacySseTransportConfig) -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("build reqwest client");
        Self(WorkerTransport::spawn(LegacySseWorker { client, config }))
    }
}

impl Transport<RoleClient> for LegacySseClientTransport {
    type Error = LegacySseError;

    fn send(
        &mut self,
        item: rmcp::service::TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.0.send(item)
    }

    fn receive(
        &mut self,
    ) -> impl Future<Output = Option<rmcp::service::RxJsonRpcMessage<RoleClient>>> + Send {
        self.0.receive()
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.0.close()
    }
}

#[derive(Clone, Debug)]
struct LegacySseWorker {
    client: reqwest::Client,
    config: LegacySseTransportConfig,
}

impl Worker for LegacySseWorker {
    type Error = LegacySseError;
    type Role = RoleClient;

    fn err_closed() -> Self::Error {
        LegacySseError::TransportChannelClosed
    }

    fn err_join(e: tokio::task::JoinError) -> Self::Error {
        LegacySseError::Join(e)
    }

    fn config(&self) -> WorkerConfig {
        WorkerConfig {
            name: Some("LegacySseWorker".into()),
            channel_buffer_capacity: self.config.channel_buffer_capacity,
        }
    }

    async fn run(
        self,
        mut context: WorkerContext<Self>,
    ) -> Result<(), WorkerQuitReason<Self::Error>> {
        let (response_tx, mut response_rx) =
            tokio::sync::mpsc::channel::<ServerJsonRpcMessage>(self.config.channel_buffer_capacity);
        let (notification_tx, mut notification_rx) =
            tokio::sync::mpsc::channel::<ServerJsonRpcMessage>(self.config.channel_buffer_capacity);
        let (endpoint_tx, endpoint_rx) = tokio::sync::watch::channel::<Option<Arc<str>>>(None);
        let transport_ct = context.cancellation_token.clone();
        let mut stream_task = tokio::spawn(run_sse_loop(
            self.client.clone(),
            self.config.clone(),
            endpoint_tx,
            response_tx,
            notification_tx,
            transport_ct.child_token(),
        ));

        let mut endpoint_rx = endpoint_rx;
        let endpoint = wait_for_endpoint(&mut endpoint_rx, self.config.endpoint_wait_timeout)
            .await
            .map_err(WorkerQuitReason::fatal_context(
                "wait for legacy SSE endpoint",
            ))?;

        let initialize = context.recv_from_handler().await?;
        post_message(
            &self.client,
            endpoint.clone(),
            self.config.auth_token.clone(),
            initialize.message,
        )
        .await
        .map_err(WorkerQuitReason::fatal_context(
            "send initialize over legacy SSE",
        ))?;
        let _ = initialize.responder.send(Ok(()));

        // Legacy SSE servers can send notifications before the initialize
        // response arrives; buffer them so startup stays lossless.
        let mut buffered_preinitialize_messages = VecDeque::new();
        let initialize_response = loop {
            tokio::select! {
                response = response_rx.recv() => {
                    let Some(message) = response else {
                        return Err(WorkerQuitReason::fatal(
                            LegacySseError::TransportChannelClosed,
                            "legacy SSE response channel closed before initialize response",
                        ));
                    };
                    break message;
                }
                notification = notification_rx.recv() => {
                    let Some(message) = notification else {
                        return Err(WorkerQuitReason::fatal(
                            LegacySseError::TransportChannelClosed,
                            "legacy SSE notification channel closed before initialize response",
                        ));
                    };
                    buffered_preinitialize_messages.push_back(message);
                }
            }
        };
        context.send_to_handler(initialize_response).await?;
        while let Some(message) = buffered_preinitialize_messages.pop_front() {
            deliver_notification_best_effort(&context, message)?;
        }

        loop {
            tokio::select! {
                _ = transport_ct.cancelled() => {
                    return Err(WorkerQuitReason::Cancelled);
                }
                message = context.recv_from_handler() => {
                    let send = message?;
                    let endpoint = endpoint_rx.borrow().clone().ok_or_else(|| {
                        WorkerQuitReason::fatal(LegacySseError::MissingEndpoint, "legacy SSE endpoint unavailable")
                    })?;
                    let response = post_message(
                        &self.client,
                        endpoint,
                        self.config.auth_token.clone(),
                        send.message,
                    ).await;
                    let _ = send.responder.send(response.map(|_| ()));
                }
                response = response_rx.recv() => {
                    let Some(server_message) = response else {
                        return Err(WorkerQuitReason::fatal(
                            LegacySseError::TransportChannelClosed,
                            "legacy SSE response channel closed",
                        ));
                    };
                    context.send_to_handler(server_message).await?;
                }
                notification = notification_rx.recv() => {
                    let Some(server_message) = notification else {
                        return Err(WorkerQuitReason::fatal(
                            LegacySseError::TransportChannelClosed,
                            "legacy SSE notification channel closed",
                        ));
                    };
                    deliver_notification_best_effort(&context, server_message)?;
                }
                result = &mut stream_task => {
                    let result = result.map_err(WorkerQuitReason::Join)?;
                    return result;
                }
            }
        }
    }
}

fn is_response_message(message: &ServerJsonRpcMessage) -> bool {
    matches!(
        message,
        ServerJsonRpcMessage::Response(_) | ServerJsonRpcMessage::Error(_)
    )
}

fn deliver_notification_best_effort(
    context: &WorkerContext<LegacySseWorker>,
    message: ServerJsonRpcMessage,
) -> Result<(), WorkerQuitReason<LegacySseError>> {
    match context.to_handler_tx.try_send(message) {
        Ok(()) => Ok(()),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!(
                "dropping legacy SSE notification due to downstream handler backpressure"
            );
            Ok(())
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            Err(WorkerQuitReason::HandlerTerminated)
        }
    }
}

async fn run_sse_loop(
    client: reqwest::Client,
    config: LegacySseTransportConfig,
    endpoint_tx: tokio::sync::watch::Sender<Option<Arc<str>>>,
    response_tx: tokio::sync::mpsc::Sender<ServerJsonRpcMessage>,
    notification_tx: tokio::sync::mpsc::Sender<ServerJsonRpcMessage>,
    ct: tokio_util::sync::CancellationToken,
) -> Result<(), WorkerQuitReason<LegacySseError>> {
    let mut last_event_id: Option<String> = None;
    let mut retry_count = 0usize;

    loop {
        let mut stream = match open_sse_stream(
            client.clone(),
            config.uri.clone(),
            config.auth_token.clone(),
            last_event_id.clone(),
        )
        .await
        {
            Ok(stream) => {
                retry_count = 0;
                stream
            }
            Err(error) => {
                let Some(delay) = config.retry_policy.retry(retry_count) else {
                    return Err(WorkerQuitReason::fatal(error, "open legacy SSE stream"));
                };
                retry_count += 1;
                tokio::select! {
                    _ = ct.cancelled() => return Err(WorkerQuitReason::Cancelled),
                    _ = tokio::time::sleep(delay) => continue,
                }
            }
        };

        loop {
            tokio::select! {
                _ = ct.cancelled() => return Err(WorkerQuitReason::Cancelled),
                event = stream.next() => {
                    match event {
                        Some(Ok(event)) => {
                            if let Some(ref id) = event.id {
                                last_event_id = Some(id.clone());
                            }
                            if event.event.as_deref() == Some("endpoint") {
                                let Some(data) = event.data.as_deref() else {
                                    return Err(WorkerQuitReason::fatal(LegacySseError::MissingEndpoint, "process endpoint event"));
                                };
                                let endpoint = resolve_endpoint(config.uri.as_ref(), data)
                                    .map_err(WorkerQuitReason::fatal_context("resolve endpoint event"))?;
                                endpoint_tx.send_replace(Some(endpoint));
                                continue;
                            }
                            let is_message_event = matches!(event.event.as_deref(), None | Some("") | Some("message"));
                            if !is_message_event {
                                continue;
                            }
                            let Some(data) = event.data else {
                                continue;
                            };
                            let message = match serde_json::from_str::<ServerJsonRpcMessage>(&data) {
                                Ok(message) => message,
                                Err(error) => {
                                    tracing::debug!(error = %error, "failed to deserialize legacy SSE server message");
                                    continue;
                                }
                            };
                            if is_response_message(&message) {
                                response_tx
                                    .send(message)
                                    .await
                                    .map_err(|_| WorkerQuitReason::HandlerTerminated)?;
                            } else if notification_tx.try_send(message).is_err() {
                                tracing::warn!("dropping legacy SSE notification due to worker backpressure");
                            }
                        }
                        Some(Err(error)) => {
                            let Some(delay) = config.retry_policy.retry(retry_count) else {
                                return Err(WorkerQuitReason::fatal(error.into(), "process legacy SSE stream"));
                            };
                            retry_count += 1;
                            tokio::select! {
                                _ = ct.cancelled() => return Err(WorkerQuitReason::Cancelled),
                                _ = tokio::time::sleep(delay) => break,
                            }
                        }
                        None => {
                            let Some(delay) = config.retry_policy.retry(retry_count) else {
                                return Err(WorkerQuitReason::TransportClosed);
                            };
                            retry_count += 1;
                            tokio::select! {
                                _ = ct.cancelled() => return Err(WorkerQuitReason::Cancelled),
                                _ = tokio::time::sleep(delay) => break,
                            }
                        }
                    }
                }
            }
        }
    }
}

async fn wait_for_endpoint(
    endpoint_rx: &mut tokio::sync::watch::Receiver<Option<Arc<str>>>,
    timeout: Duration,
) -> Result<Arc<str>, LegacySseError> {
    if let Some(endpoint) = endpoint_rx.borrow().clone() {
        return Ok(endpoint);
    }

    tokio::time::timeout(timeout, async {
        loop {
            endpoint_rx
                .changed()
                .await
                .map_err(|_| LegacySseError::MissingEndpoint)?;
            if let Some(endpoint) = endpoint_rx.borrow().clone() {
                return Ok(endpoint);
            }
        }
    })
    .await
    .map_err(|_| LegacySseError::MissingEndpoint)?
}

async fn open_sse_stream(
    client: reqwest::Client,
    uri: Arc<str>,
    auth_token: Option<Arc<str>>,
    last_event_id: Option<String>,
) -> Result<BoxedSseStream, LegacySseError> {
    let mut request = client
        .get(uri.as_ref())
        .header(header::ACCEPT, "text/event-stream");
    if let Some(token) = auth_token {
        request = request.bearer_auth(token.as_ref());
    }
    if let Some(last_event_id) = last_event_id {
        request = request.header(HeaderName::from_static("last-event-id"), last_event_id);
    }

    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(LegacySseError::UnexpectedStatus { status, body });
    }

    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string());
    if !content_type
        .as_deref()
        .is_some_and(|value| value.starts_with("text/event-stream"))
    {
        return Err(LegacySseError::UnexpectedContentType(content_type));
    }

    Ok(SseStream::from_byte_stream(response.bytes_stream()).boxed())
}

async fn post_message(
    client: &reqwest::Client,
    endpoint: Arc<str>,
    auth_token: Option<Arc<str>>,
    message: ClientJsonRpcMessage,
) -> Result<(), LegacySseError> {
    let mut request = client
        .post(endpoint.as_ref())
        .timeout(Duration::from_secs(30))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(token) = auth_token {
        request = request.bearer_auth(token.as_ref());
    }

    let response = request.json(&message).send().await?;
    let status = response.status();
    if matches!(
        status,
        StatusCode::OK | StatusCode::ACCEPTED | StatusCode::NO_CONTENT
    ) {
        return Ok(());
    }

    let body = response.text().await.unwrap_or_default();
    Err(LegacySseError::UnexpectedStatus { status, body })
}

fn resolve_endpoint(base_url: &str, endpoint: &str) -> Result<Arc<str>, LegacySseError> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return Err(LegacySseError::MissingEndpoint);
    }

    let base = reqwest::Url::parse(base_url)
        .map_err(|_| LegacySseError::InvalidEndpoint(trimmed.into()))?;
    let joined = match reqwest::Url::parse(trimmed) {
        Ok(url) => url,
        Err(_) => base
            .join(trimmed)
            .map_err(|_| LegacySseError::InvalidEndpoint(trimmed.into()))?,
    };

    // Enforce same-origin: the resolved endpoint must share the scheme, host, and port
    // of the base URL. This prevents a malicious server from redirecting traffic
    // to arbitrary destinations via the SSE `endpoint` event.
    if joined.scheme() != base.scheme()
        || joined.host_str() != base.host_str()
        || joined.port() != base.port()
    {
        return Err(LegacySseError::InvalidEndpoint(format!(
            "endpoint '{}' does not match origin of '{}'",
            joined, base_url
        )));
    }

    Ok(Arc::from(joined.to_string()))
}

/// Check if an HTTP connection error suggests the remote server uses legacy SSE
/// rather than Streamable HTTP. Used by the HTTP→SSE fallback path.
pub fn should_fallback_http_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<LegacySseError>()
            .is_some_and(LegacySseError::is_legacy_fallback_hint)
            || cause
                .to_string()
                .contains("unexpected server response: HTTP 405 Method Not Allowed")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use std::sync::Mutex;

    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::sse::{Event, Sse};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use futures::Stream;
    use rmcp::ServiceExt as _;
    use rmcp::handler::client::ClientHandler;
    use rmcp::model::{
        CallToolRequestParams, ClientRequest, Content, Implementation, InitializeResult,
        ListToolsResult, LoggingLevel, LoggingMessageNotification, LoggingMessageNotificationParam,
        ServerCapabilities, ServerNotification, ServerResult, Tool, ToolsCapability,
    };
    use rmcp::service::NotificationContext;
    use serde_json::json;
    use tokio::sync::Notify;

    #[test]
    fn resolve_endpoint_relative_path() {
        let endpoint =
            resolve_endpoint("http://localhost:8080/mcp", "/messages").expect("resolve relative");
        assert_eq!(endpoint.as_ref(), "http://localhost:8080/messages");
    }

    #[tokio::test]
    async fn wait_for_endpoint_uses_provided_timeout() {
        let (_tx, mut rx) = tokio::sync::watch::channel::<Option<Arc<str>>>(None);
        let result = wait_for_endpoint(&mut rx, Duration::from_millis(10)).await;
        assert!(matches!(result, Err(LegacySseError::MissingEndpoint)));
    }

    #[test]
    fn generic_http_status_strings_do_not_trigger_fallback() {
        let error = anyhow::anyhow!("failed to connect to HTTP upstream: HTTP 404 Not Found");
        assert!(!should_fallback_http_error(&error));
    }

    #[test]
    fn resolve_endpoint_same_origin_absolute() {
        let endpoint = resolve_endpoint(
            "http://localhost:8080/mcp",
            "http://localhost:8080/messages",
        )
        .expect("resolve same-origin absolute");
        assert_eq!(endpoint.as_ref(), "http://localhost:8080/messages");
    }

    #[test]
    fn resolve_endpoint_rejects_cross_origin() {
        let result = resolve_endpoint("http://localhost:8080/mcp", "https://evil.com/steal");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not match origin"), "got: {err}");
    }

    #[test]
    fn resolve_endpoint_rejects_metadata_redirect() {
        let result = resolve_endpoint(
            "http://localhost:8080/mcp",
            "http://169.254.169.254/latest/meta-data/",
        );
        assert!(result.is_err());
    }

    #[test]
    fn resolve_endpoint_rejects_scheme_downgrade() {
        let result = resolve_endpoint("https://example.com/mcp", "http://example.com/messages");
        assert!(result.is_err());
    }

    #[test]
    fn resolve_endpoint_rejects_port_change() {
        let result = resolve_endpoint(
            "http://localhost:8080/mcp",
            "http://localhost:9090/messages",
        );
        assert!(result.is_err());
    }

    #[test]
    fn resolve_endpoint_empty_returns_error() {
        let result = resolve_endpoint("http://localhost:8080/mcp", "  ");
        assert!(result.is_err());
    }

    #[test]
    fn fallback_hint_detects_status_codes() {
        assert!(
            LegacySseError::UnexpectedStatus {
                status: StatusCode::METHOD_NOT_ALLOWED,
                body: String::new(),
            }
            .is_legacy_fallback_hint()
        );

        assert!(
            LegacySseError::UnexpectedStatus {
                status: StatusCode::NOT_FOUND,
                body: String::new(),
            }
            .is_legacy_fallback_hint()
        );

        assert!(
            !LegacySseError::UnexpectedStatus {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                body: String::new(),
            }
            .is_legacy_fallback_hint()
        );
    }

    #[test]
    fn debug_redacts_auth_token() {
        let config = LegacySseTransportConfig::with_uri("http://localhost:8080/mcp")
            .auth_token("super-secret-token");
        let debug_output = format!("{config:?}");
        assert!(
            !debug_output.contains("super-secret"),
            "auth token should be redacted in Debug output"
        );
        assert!(
            debug_output.contains("REDACTED"),
            "should show REDACTED placeholder"
        );
    }

    #[derive(Clone)]
    struct EarlyNotificationState {
        tx: tokio::sync::broadcast::Sender<sse_stream::Sse>,
    }

    #[derive(Clone)]
    struct LoggingCaptureClient {
        signal: Arc<Notify>,
        payloads: Arc<Mutex<Vec<serde_json::Value>>>,
        notification_delay: Duration,
    }

    impl ClientHandler for LoggingCaptureClient {
        fn get_info(&self) -> rmcp::model::ClientInfo {
            rmcp::model::ClientInfo::default()
        }

        async fn on_logging_message(
            &self,
            params: LoggingMessageNotificationParam,
            _context: NotificationContext<rmcp::RoleClient>,
        ) {
            if !self.notification_delay.is_zero() {
                tokio::time::sleep(self.notification_delay).await;
            }
            self.payloads.lock().unwrap().push(params.data);
            self.signal.notify_one();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn preinitialize_notifications_are_replayed_after_initialize_response() {
        crate::tls::ensure_rustls_provider_installed();
        let (server_url, _server_handle) = spawn_legacy_sse_server_with_early_logging().await;
        let signal = Arc::new(Notify::new());
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(LoggingCaptureClient {
            signal: Arc::clone(&signal),
            payloads: Arc::clone(&payloads),
            notification_delay: Duration::ZERO,
        });

        let transport = LegacySseClientTransport::from_config(
            LegacySseTransportConfig::with_uri(server_url)
                .endpoint_wait_timeout(Duration::from_secs(1)),
        );

        let _client = handler
            .serve(transport)
            .await
            .expect("connect legacy SSE client");

        tokio::time::timeout(Duration::from_secs(2), signal.notified())
            .await
            .expect("receive early logging notification");
        assert_eq!(
            payloads.lock().unwrap().as_slice(),
            &[json!({"phase": "before_initialize_response"})]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn responses_are_not_blocked_by_notification_backlog() {
        crate::tls::ensure_rustls_provider_installed();
        let (server_url, _server_handle) = spawn_legacy_sse_server_with_noisy_call().await;
        let handler = Arc::new(LoggingCaptureClient {
            signal: Arc::new(Notify::new()),
            payloads: Arc::new(Mutex::new(Vec::new())),
            notification_delay: Duration::from_millis(50),
        });
        let transport = LegacySseClientTransport::from_config(
            LegacySseTransportConfig::with_uri(server_url)
                .endpoint_wait_timeout(Duration::from_secs(1)),
        );
        let client = handler
            .serve(transport)
            .await
            .expect("connect noisy legacy SSE client");

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            client.peer().call_tool(CallToolRequestParams::new("echo")),
        )
        .await
        .expect("tool response should not stall behind notifications")
        .expect("call tool over noisy legacy SSE");

        assert!(
            format!("{result:?}").contains("noisy call completed"),
            "unexpected tool response: {result:?}"
        );
    }

    async fn spawn_legacy_sse_server_with_early_logging() -> (String, tokio::task::JoinHandle<()>) {
        let (tx, _) = tokio::sync::broadcast::channel(16);
        let state = EarlyNotificationState { tx };
        let app = Router::new()
            .route("/mcp", get(early_logging_stream))
            .route("/messages", post(early_logging_messages))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind legacy SSE early logging server");
        let addr = listener
            .local_addr()
            .expect("legacy SSE early logging addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve legacy SSE early logging server");
        });

        (format!("http://127.0.0.1:{}/mcp", addr.port()), handle)
    }

    async fn spawn_legacy_sse_server_with_noisy_call() -> (String, tokio::task::JoinHandle<()>) {
        let (tx, _) = tokio::sync::broadcast::channel(64);
        let state = EarlyNotificationState { tx };
        let app = Router::new()
            .route("/mcp", get(early_logging_stream))
            .route("/messages", post(noisy_call_messages))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind noisy legacy SSE server");
        let addr = listener.local_addr().expect("noisy legacy SSE addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve noisy legacy SSE server");
        });

        (format!("http://127.0.0.1:{}/mcp", addr.port()), handle)
    }

    async fn early_logging_stream(
        State(state): State<EarlyNotificationState>,
    ) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
        let mut rx = state.tx.subscribe();
        let stream = async_stream::stream! {
            yield Ok(Event::default().event("endpoint").data("/messages"));
            loop {
                match rx.recv().await {
                    Ok(event) => yield Ok(event_to_axum(event)),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        };
        Ok(Sse::new(stream))
    }

    async fn early_logging_messages(
        State(state): State<EarlyNotificationState>,
        Json(message): Json<ClientJsonRpcMessage>,
    ) -> StatusCode {
        if let ClientJsonRpcMessage::Request(request) = message {
            match request.request {
                ClientRequest::InitializeRequest(_) => {
                    let mut capabilities = ServerCapabilities::default();
                    capabilities.tools = Some(ToolsCapability {
                        list_changed: Some(false),
                    });
                    let logging = ServerJsonRpcMessage::notification(
                        ServerNotification::LoggingMessageNotification(
                            LoggingMessageNotification::new(LoggingMessageNotificationParam::new(
                                LoggingLevel::Info,
                                json!({"phase": "before_initialize_response"}),
                            )),
                        ),
                    );
                    let response = ServerJsonRpcMessage::response(
                        ServerResult::InitializeResult(
                            InitializeResult::new(capabilities)
                                .with_server_info(Implementation::new("legacy-sse-test", "1.0")),
                        ),
                        request.id,
                    );
                    let _ = state.tx.send(sse_stream::Sse::default().data(
                        serde_json::to_string(&logging).expect("serialize logging notification"),
                    ));
                    let _ = state.tx.send(sse_stream::Sse::default().data(
                        serde_json::to_string(&response).expect("serialize initialize response"),
                    ));
                }
                ClientRequest::ListToolsRequest(_) => {
                    let response = ServerJsonRpcMessage::response(
                        ServerResult::ListToolsResult(ListToolsResult::with_all_items(
                            Vec::<Tool>::new(),
                        )),
                        request.id,
                    );
                    let _ = state.tx.send(sse_stream::Sse::default().data(
                        serde_json::to_string(&response).expect("serialize list tools response"),
                    ));
                }
                _ => {}
            }
        }
        StatusCode::ACCEPTED
    }

    async fn noisy_call_messages(
        State(state): State<EarlyNotificationState>,
        Json(message): Json<ClientJsonRpcMessage>,
    ) -> StatusCode {
        if let ClientJsonRpcMessage::Request(request) = message {
            match request.request {
                ClientRequest::InitializeRequest(_) => {
                    let mut capabilities = ServerCapabilities::default();
                    capabilities.tools = Some(ToolsCapability {
                        list_changed: Some(false),
                    });
                    let response = ServerJsonRpcMessage::response(
                        ServerResult::InitializeResult(
                            InitializeResult::new(capabilities)
                                .with_server_info(Implementation::new("legacy-sse-test", "1.0")),
                        ),
                        request.id,
                    );
                    let _ = state.tx.send(sse_stream::Sse::default().data(
                        serde_json::to_string(&response).expect("serialize initialize response"),
                    ));
                }
                ClientRequest::ListToolsRequest(_) => {
                    let mut tool = Tool::default();
                    tool.name = "echo".into();
                    tool.description = Some("Echo".into());
                    tool.input_schema = Arc::new(serde_json::Map::new());
                    let response = ServerJsonRpcMessage::response(
                        ServerResult::ListToolsResult(ListToolsResult::with_all_items(vec![tool])),
                        request.id,
                    );
                    let _ = state.tx.send(sse_stream::Sse::default().data(
                        serde_json::to_string(&response).expect("serialize list tools response"),
                    ));
                }
                ClientRequest::CallToolRequest(_) => {
                    for idx in 0..32 {
                        let logging = ServerJsonRpcMessage::notification(
                            ServerNotification::LoggingMessageNotification(
                                LoggingMessageNotification::new(
                                    LoggingMessageNotificationParam::new(
                                        LoggingLevel::Info,
                                        json!({"burst": idx}),
                                    ),
                                ),
                            ),
                        );
                        let _ = state.tx.send(
                            sse_stream::Sse::default().data(
                                serde_json::to_string(&logging)
                                    .expect("serialize noisy logging notification"),
                            ),
                        );
                    }
                    let response = ServerJsonRpcMessage::response(
                        ServerResult::CallToolResult(rmcp::model::CallToolResult::success(vec![
                            Content::text("noisy call completed"),
                        ])),
                        request.id,
                    );
                    let _ =
                        state.tx.send(sse_stream::Sse::default().data(
                            serde_json::to_string(&response).expect("serialize tool response"),
                        ));
                }
                _ => {}
            }
        }
        StatusCode::ACCEPTED
    }

    fn event_to_axum(event: sse_stream::Sse) -> Event {
        let mut axum_event = Event::default();
        if let Some(kind) = event.event {
            axum_event = axum_event.event(kind);
        }
        if let Some(data) = event.data {
            axum_event = axum_event.data(data);
        }
        if let Some(id) = event.id {
            axum_event = axum_event.id(id);
        }
        axum_event
    }
}
