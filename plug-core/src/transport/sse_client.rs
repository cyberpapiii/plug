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

#[derive(Clone, Debug)]
pub struct LegacySseTransportConfig {
    pub uri: Arc<str>,
    pub auth_token: Option<Arc<str>>,
    pub channel_buffer_capacity: usize,
    pub retry_policy: Arc<dyn SseRetryPolicy>,
}

impl LegacySseTransportConfig {
    pub fn with_uri(uri: impl Into<Arc<str>>) -> Self {
        Self {
            uri: uri.into(),
            auth_token: None,
            channel_buffer_capacity: 16,
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
}

pub struct LegacySseClientTransport(WorkerTransport<LegacySseWorker>);

impl LegacySseClientTransport {
    pub fn from_config(config: LegacySseTransportConfig) -> Self {
        Self(WorkerTransport::spawn(LegacySseWorker {
            client: reqwest::Client::default(),
            config,
        }))
    }

    pub fn from_uri(uri: impl Into<Arc<str>>) -> Self {
        Self::from_config(LegacySseTransportConfig::with_uri(uri))
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
        let (sse_tx, mut sse_rx) =
            tokio::sync::mpsc::channel::<ServerJsonRpcMessage>(self.config.channel_buffer_capacity);
        let (endpoint_tx, endpoint_rx) = tokio::sync::watch::channel::<Option<Arc<str>>>(None);
        let transport_ct = context.cancellation_token.clone();
        let mut stream_task = tokio::spawn(run_sse_loop(
            self.client.clone(),
            self.config.clone(),
            endpoint_tx,
            sse_tx.clone(),
            transport_ct.child_token(),
        ));

        let mut endpoint_rx = endpoint_rx;
        let endpoint =
            wait_for_endpoint(&mut endpoint_rx)
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

        let initialize_response = loop {
            let Some(message) = sse_rx.recv().await else {
                return Err(WorkerQuitReason::fatal(
                    LegacySseError::TransportChannelClosed,
                    "legacy SSE stream closed before initialize response",
                ));
            };
            if matches!(message, ServerJsonRpcMessage::Response(_)) {
                break message;
            }
        };
        context.send_to_handler(initialize_response).await?;

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
                server_message = sse_rx.recv() => {
                    let Some(server_message) = server_message else {
                        return Err(WorkerQuitReason::fatal(
                            LegacySseError::TransportChannelClosed,
                            "legacy SSE message channel closed",
                        ));
                    };
                    context.send_to_handler(server_message).await?;
                }
                result = &mut stream_task => {
                    let result = result.map_err(WorkerQuitReason::Join)?;
                    return result;
                }
            }
        }
    }
}

async fn run_sse_loop(
    client: reqwest::Client,
    config: LegacySseTransportConfig,
    endpoint_tx: tokio::sync::watch::Sender<Option<Arc<str>>>,
    sse_tx: tokio::sync::mpsc::Sender<ServerJsonRpcMessage>,
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
                            sse_tx.send(message).await.map_err(|_| WorkerQuitReason::HandlerTerminated)?;
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
) -> Result<Arc<str>, LegacySseError> {
    if let Some(endpoint) = endpoint_rx.borrow().clone() {
        return Ok(endpoint);
    }

    tokio::time::timeout(Duration::from_secs(5), async {
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
    Ok(Arc::from(joined.to_string()))
}

/// Check if an HTTP connection error suggests the remote server uses legacy SSE
/// rather than Streamable HTTP. Used by the HTTP→SSE fallback path.
pub fn should_fallback_http_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<LegacySseError>())
        .any(LegacySseError::is_legacy_fallback_hint)
        || error.to_string().contains("HTTP 400")
        || error.to_string().contains("HTTP 404")
        || error.to_string().contains("HTTP 405")
}
