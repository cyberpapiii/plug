use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use futures::{StreamExt, stream::BoxStream};
use http::{HeaderName, header};
use reqwest::StatusCode;
use rmcp::RoleClient;
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::worker::{Worker, WorkerConfig, WorkerContext, WorkerQuitReason};
use rmcp::transport::{Transport, WorkerTransport};
use sse_stream::SseStream;

use crate::backoff::Backoff;

type BoxedSseStream = BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>;

const CHANNEL_CAPACITY: usize = 16;

/// How long a message POST may take unless the server's config says.
const POST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LegacySseError {
    #[error("reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("stream closed before endpoint event")]
    MissingEndpoint,
    #[error("invalid endpoint event payload: {0}")]
    InvalidEndpoint(String),
    #[error("unexpected SSE response status {status}: {body}")]
    UnexpectedStatus {
        status: StatusCode,
        /// The response's `WWW-Authenticate` value.
        challenge: Option<String>,
        body: String,
    },
    #[error("unexpected SSE content type: {0:?}")]
    UnexpectedContentType(Option<String>),
    #[error("transport channel closed")]
    TransportChannelClosed,
    #[error("join error: {0}")]
    Join(#[from] tokio::task::JoinError),
}

impl LegacySseError {
    /// The server refused Plug's credentials when it opened the stream.
    pub fn refuses_credentials(&self) -> bool {
        matches!(
            self,
            Self::UnexpectedStatus { status, challenge, .. }
                if super::refusal_needs_sign_in(status.as_u16(), challenge.as_deref())
        )
    }
}

#[derive(Clone)]
pub struct LegacySseTransportConfig {
    pub uri: Arc<str>,
    pub auth_token: Option<Arc<str>>,
    pub endpoint_wait_timeout: Duration,
    /// Bound on each message POST. The server answers on the stream, so the
    /// POST itself only hands the message over.
    pub post_timeout: Duration,
    /// Called when the server re-advertises a *different* POST endpoint after a
    /// stream reconnect.
    ///
    /// The retry loop reconnects on its own and never rebuilds anything above
    /// it, so without this the layers that hold per-session state — resource
    /// subscriptions above all — have no way to learn the session they
    /// registered against is gone. An unchanged endpoint is left silent on
    /// purpose: `last-event-id` is sent on every redial, and a server that
    /// answers with the same endpoint is resuming, not minting a new session.
    pub session_observer: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for LegacySseTransportConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LegacySseTransportConfig")
            .field("uri", &self.uri)
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("endpoint_wait_timeout", &self.endpoint_wait_timeout)
            .field("post_timeout", &self.post_timeout)
            .finish()
    }
}

impl LegacySseTransportConfig {
    pub fn with_uri(uri: impl Into<Arc<str>>) -> Self {
        Self {
            uri: uri.into(),
            auth_token: None,
            endpoint_wait_timeout: Duration::from_secs(5),
            post_timeout: POST_TIMEOUT,
            session_observer: None,
        }
    }

    pub fn session_observer(mut self, observer: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.session_observer = Some(observer);
        self
    }

    pub fn auth_token(mut self, token: impl Into<Arc<str>>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    pub fn endpoint_wait_timeout(mut self, timeout: Duration) -> Self {
        self.endpoint_wait_timeout = timeout;
        self
    }

    pub fn post_timeout(mut self, timeout: Duration) -> Self {
        self.post_timeout = timeout;
        self
    }
}

pub struct LegacySseClientTransport(WorkerTransport<LegacySseWorker>);

impl LegacySseClientTransport {
    /// Open the stream, then hand it to the worker. A server that refuses the
    /// first open fails here with its status, where a failure inside the
    /// worker would reach the caller only as a closed channel.
    pub async fn connect(config: LegacySseTransportConfig) -> anyhow::Result<Self> {
        let client = crate::upstream_http::client(crate::upstream_http::Redirects::Never)?;
        let stream = open_sse_stream(
            client.clone(),
            config.uri.clone(),
            config.auth_token.clone(),
            None,
        )
        .await?;
        Ok(Self(WorkerTransport::spawn(LegacySseWorker {
            client,
            config,
            stream,
        })))
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

struct LegacySseWorker {
    client: reqwest::Client,
    config: LegacySseTransportConfig,
    stream: BoxedSseStream,
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
        let mut config = WorkerConfig::default();
        config.name = Some("LegacySseWorker".into());
        config.channel_buffer_capacity = CHANNEL_CAPACITY;
        config
    }

    async fn run(
        self,
        mut context: WorkerContext<Self>,
    ) -> Result<(), WorkerQuitReason<Self::Error>> {
        let (response_tx, mut response_rx) =
            tokio::sync::mpsc::channel::<ServerJsonRpcMessage>(CHANNEL_CAPACITY);
        let (notification_tx, mut notification_rx) =
            tokio::sync::mpsc::channel::<ServerJsonRpcMessage>(CHANNEL_CAPACITY);
        let (endpoint_tx, endpoint_rx) = tokio::sync::watch::channel::<Option<Arc<str>>>(None);
        let transport_ct = context.cancellation_token.clone();
        let mut stream_task = tokio::spawn(run_sse_loop(
            self.client.clone(),
            self.config.clone(),
            self.stream,
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
        if let Err(error) = post_message(
            &self.client,
            endpoint.clone(),
            &self.config,
            initialize.message,
        )
        .await
        {
            // rmcp reports a dropped responder as a closed channel, so the
            // refusal, and its status, reach the caller only through it.
            let _ = initialize.responder.send(Err(error));
            return Err(WorkerQuitReason::fatal(
                LegacySseError::TransportChannelClosed,
                "send initialize over legacy SSE",
            ));
        }
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
                        &self.config,
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

/// Publish a newly advertised POST endpoint, notifying the session observer if
/// it replaced a different one.
///
/// A changed endpoint is the only evidence this transport gets that a reconnect
/// produced a new server session rather than a resumed one. The first endpoint
/// of a connection replaces nothing and is therefore never a replacement, and a
/// redial answered with the same endpoint is treated as a resume — the redial
/// carries `last-event-id`, so a server that meant to start over would have to
/// say so by moving the endpoint.
fn publish_endpoint(
    endpoint_tx: &tokio::sync::watch::Sender<Option<Arc<str>>>,
    endpoint: Arc<str>,
    session_observer: Option<&Arc<dyn Fn() + Send + Sync>>,
) {
    let previous = endpoint_tx.send_replace(Some(endpoint.clone()));
    if let (Some(previous), Some(observer)) = (previous, session_observer)
        && previous != endpoint
    {
        tracing::info!(
            "legacy SSE server re-advertised a new POST endpoint; treating as a new connection"
        );
        observer();
    }
}

async fn run_sse_loop(
    client: reqwest::Client,
    config: LegacySseTransportConfig,
    first_stream: BoxedSseStream,
    endpoint_tx: tokio::sync::watch::Sender<Option<Arc<str>>>,
    response_tx: tokio::sync::mpsc::Sender<ServerJsonRpcMessage>,
    notification_tx: tokio::sync::mpsc::Sender<ServerJsonRpcMessage>,
    ct: tokio_util::sync::CancellationToken,
) -> Result<(), WorkerQuitReason<LegacySseError>> {
    let mut last_event_id: Option<String> = None;
    let mut retry_count = 0u32;
    let mut first_stream = Some(first_stream);

    loop {
        let opened = match first_stream.take() {
            Some(stream) => Ok(stream),
            None => {
                open_sse_stream(
                    client.clone(),
                    config.uri.clone(),
                    config.auth_token.clone(),
                    last_event_id.clone(),
                )
                .await
            }
        };
        let mut stream = match opened {
            Ok(stream) => {
                retry_count = 0;
                stream
            }
            // Redialing with credentials the server refuses never succeeds.
            // Ending the session lets recovery reconnect, and that first open
            // reports the refusal.
            Err(error) if error.refuses_credentials() => {
                return Err(WorkerQuitReason::fatal(error, "reopen legacy SSE stream"));
            }
            Err(error) => {
                tracing::debug!(error = %error, "could not open legacy SSE stream; redialing");
                let delay = Backoff::STREAM.delay(retry_count);
                retry_count = retry_count.saturating_add(1);
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
                                publish_endpoint(&endpoint_tx, endpoint, config.session_observer.as_ref());
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
                            tracing::debug!(error = %error, "legacy SSE stream failed; redialing");
                            let delay = Backoff::STREAM.delay(retry_count);
                            retry_count = retry_count.saturating_add(1);
                            tokio::select! {
                                _ = ct.cancelled() => return Err(WorkerQuitReason::Cancelled),
                                _ = tokio::time::sleep(delay) => break,
                            }
                        }
                        None => {
                            let delay = Backoff::STREAM.delay(retry_count);
                            retry_count = retry_count.saturating_add(1);
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
        return Err(unexpected_status(response).await);
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

    Ok(SseStream::from_bytes_stream(response.bytes_stream()).boxed())
}

async fn post_message(
    client: &reqwest::Client,
    endpoint: Arc<str>,
    config: &LegacySseTransportConfig,
    message: ClientJsonRpcMessage,
) -> Result<(), LegacySseError> {
    let mut request = client
        .post(endpoint.as_ref())
        .timeout(config.post_timeout)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(token) = &config.auth_token {
        request = request.bearer_auth(token.as_ref());
    }
    for (name, value) in crate::mcp_http_headers::mirrored_headers_for_message(&message) {
        request = request.header(name, value);
    }

    let response = request.json(&message).send().await?;
    let status = response.status();
    if matches!(
        status,
        StatusCode::OK | StatusCode::ACCEPTED | StatusCode::NO_CONTENT
    ) {
        return Ok(());
    }

    Err(unexpected_status(response).await)
}

async fn unexpected_status(response: reqwest::Response) -> LegacySseError {
    let status = response.status();
    let challenge = response
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response.text().await.unwrap_or_default();
    LegacySseError::UnexpectedStatus {
        status,
        challenge,
        body,
    }
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

    // A server must not send Plug's requests, and their credential, anywhere
    // else with its `endpoint` event.
    if !crate::upstream_http::same_origin(&joined, &base) {
        return Err(LegacySseError::InvalidEndpoint(format!(
            "endpoint '{}' does not match origin of '{}'",
            joined, base_url
        )));
    }

    Ok(Arc::from(joined.to_string()))
}

/// A legacy SSE server for tests. `GET /mcp` streams the `/messages`
/// endpoint, then whatever is sent; `POST /messages` hands each message to
/// the test, which answers by sending. `POST /mcp` is refused with 405, as a
/// legacy server refuses streamable HTTP.
#[cfg(test)]
pub(crate) mod mock {
    use std::convert::Infallible;
    use std::sync::Arc;

    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::sse::{Event, Sse};
    use axum::routing::get;
    use axum::{Json, Router};
    use futures::Stream;
    use rmcp::model::{ClientJsonRpcMessage, RequestId, ServerJsonRpcMessage, ServerResult};

    type OnMessage = Arc<dyn Fn(&LegacySse, &HeaderMap, ClientJsonRpcMessage) + Send + Sync>;

    #[derive(Clone)]
    pub(crate) struct LegacySse {
        tx: tokio::sync::broadcast::Sender<sse_stream::Sse>,
    }

    #[derive(Clone)]
    struct App {
        server: LegacySse,
        bearer: Option<String>,
        on_message: OnMessage,
        refuse_messages: Option<StatusCode>,
    }

    impl App {
        fn authorized(&self, headers: &HeaderMap) -> bool {
            self.bearer.as_ref().is_none_or(|bearer| {
                headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value == format!("Bearer {bearer}"))
            })
        }
    }

    impl LegacySse {
        /// Serve on a free port, asking for `bearer` when it is given.
        /// Returns the server and its `/mcp` URL.
        pub(crate) async fn spawn(
            bearer: Option<&str>,
            on_message: impl Fn(&LegacySse, &HeaderMap, ClientJsonRpcMessage) + Send + Sync + 'static,
        ) -> (Self, String) {
            Self::serve(bearer, Arc::new(on_message), None).await
        }

        /// Serve a stream that opens, and answer every message with `status`.
        /// Returns the `/mcp` URL.
        pub(crate) async fn refusing_messages(status: StatusCode) -> String {
            Self::serve(None, Arc::new(|_, _, _| {}), Some(status))
                .await
                .1
        }

        async fn serve(
            bearer: Option<&str>,
            on_message: OnMessage,
            refuse_messages: Option<StatusCode>,
        ) -> (Self, String) {
            let server = Self {
                tx: tokio::sync::broadcast::channel(64).0,
            };
            let app = App {
                server: server.clone(),
                bearer: bearer.map(str::to_string),
                on_message,
                refuse_messages,
            };
            let router = Router::new()
                .route("/mcp", get(stream).post(refuse_streamable_http))
                .route("/messages", axum::routing::post(message))
                .with_state(app);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind legacy SSE test server");
            let port = listener.local_addr().expect("legacy SSE address").port();
            tokio::spawn(async move {
                axum::serve(listener, router)
                    .await
                    .expect("serve legacy SSE test server");
            });
            (server, format!("http://127.0.0.1:{port}/mcp"))
        }

        pub(crate) fn send(&self, message: &ServerJsonRpcMessage) {
            let data = serde_json::to_string(message).expect("serialize server message");
            let _ = self.tx.send(sse_stream::Sse::default().data(data));
        }

        pub(crate) fn respond(&self, id: RequestId, result: ServerResult) {
            self.send(&ServerJsonRpcMessage::response(result, id));
        }
    }

    async fn stream(
        State(app): State<App>,
        headers: HeaderMap,
    ) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
        if !app.authorized(&headers) {
            return Err(StatusCode::UNAUTHORIZED);
        }
        let mut rx = app.server.tx.subscribe();
        Ok(Sse::new(async_stream::stream! {
            yield Ok(Event::default().event("endpoint").data("/messages"));
            loop {
                match rx.recv().await {
                    Ok(event) => yield Ok(Event::default().data(event.data.unwrap_or_default())),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }))
    }

    async fn refuse_streamable_http(State(app): State<App>, headers: HeaderMap) -> StatusCode {
        if app.authorized(&headers) {
            StatusCode::METHOD_NOT_ALLOWED
        } else {
            StatusCode::UNAUTHORIZED
        }
    }

    async fn message(
        State(app): State<App>,
        headers: HeaderMap,
        Json(message): Json<ClientJsonRpcMessage>,
    ) -> StatusCode {
        if !app.authorized(&headers) {
            return StatusCode::UNAUTHORIZED;
        }
        if let Some(status) = app.refuse_messages {
            return status;
        }
        (app.on_message)(&app.server, &headers, message);
        StatusCode::ACCEPTED
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use rmcp::ServiceExt as _;
    use rmcp::handler::client::ClientHandler;
    use rmcp::model::{
        CallToolRequestParams, ClientRequest, ContentBlock, Implementation, InitializeResult,
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

    /// A server that never answers a POST holds a message for the server's
    /// own timeout, not a fixed one.
    #[tokio::test]
    async fn a_post_gives_up_after_the_configured_timeout() {
        let app = axum::Router::new().route(
            "/messages",
            axum::routing::post(std::future::pending::<axum::http::StatusCode>),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await });

        let config = LegacySseTransportConfig::with_uri(format!("http://127.0.0.1:{port}/mcp"))
            .post_timeout(Duration::from_millis(200));
        let client = crate::upstream_http::client(crate::upstream_http::Redirects::Never).unwrap();
        let message = ClientJsonRpcMessage::notification(
            rmcp::model::ClientNotification::InitializedNotification(Default::default()),
        );
        let started = std::time::Instant::now();
        let result = post_message(
            &client,
            Arc::from(format!("http://127.0.0.1:{port}/messages")),
            &config,
            message,
        )
        .await;
        assert!(matches!(result, Err(LegacySseError::Reqwest(error)) if error.is_timeout()));
        assert!(started.elapsed() < Duration::from_secs(5));
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
        let server_url = spawn_legacy_sse_server_with_early_logging().await;
        let signal = Arc::new(Notify::new());
        let payloads = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(LoggingCaptureClient {
            signal: Arc::clone(&signal),
            payloads: Arc::clone(&payloads),
            notification_delay: Duration::ZERO,
        });

        let transport = LegacySseClientTransport::connect(
            LegacySseTransportConfig::with_uri(server_url)
                .endpoint_wait_timeout(Duration::from_secs(1)),
        )
        .await
        .expect("open the stream");

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
        let server_url = spawn_legacy_sse_server_with_noisy_call().await;
        let handler = Arc::new(LoggingCaptureClient {
            signal: Arc::new(Notify::new()),
            payloads: Arc::new(Mutex::new(Vec::new())),
            notification_delay: Duration::from_millis(50),
        });
        let transport = LegacySseClientTransport::connect(
            LegacySseTransportConfig::with_uri(server_url)
                .endpoint_wait_timeout(Duration::from_secs(1)),
        )
        .await
        .expect("open the stream");
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

    fn initialize_result() -> ServerResult {
        let mut capabilities = ServerCapabilities::default();
        let mut tools = ToolsCapability::default();
        tools.list_changed = Some(false);
        capabilities.tools = Some(tools);
        ServerResult::InitializeResult(
            InitializeResult::new(capabilities)
                .with_server_info(Implementation::new("legacy-sse-test", "1.0")),
        )
    }

    fn logging(data: serde_json::Value) -> ServerJsonRpcMessage {
        ServerJsonRpcMessage::notification(ServerNotification::LoggingMessageNotification(
            LoggingMessageNotification::new(LoggingMessageNotificationParam::new(
                LoggingLevel::Info,
                data,
            )),
        ))
    }

    /// A server that logs before it answers `initialize`.
    async fn spawn_legacy_sse_server_with_early_logging() -> String {
        let (_, url) = mock::LegacySse::spawn(None, |server, _, message| {
            let ClientJsonRpcMessage::Request(request) = message else {
                return;
            };
            match request.request {
                ClientRequest::InitializeRequest(_) => {
                    server.send(&logging(json!({"phase": "before_initialize_response"})));
                    server.respond(request.id, initialize_result());
                }
                ClientRequest::ListToolsRequest(_) => server.respond(
                    request.id,
                    ServerResult::ListToolsResult(ListToolsResult::with_all_items(Vec::new())),
                ),
                _ => {}
            }
        })
        .await;
        url
    }

    /// A server that logs 32 times before it answers a tool call.
    async fn spawn_legacy_sse_server_with_noisy_call() -> String {
        let (_, url) = mock::LegacySse::spawn(None, |server, _, message| {
            let ClientJsonRpcMessage::Request(request) = message else {
                return;
            };
            match request.request {
                ClientRequest::InitializeRequest(_) => {
                    server.respond(request.id, initialize_result());
                }
                ClientRequest::ListToolsRequest(_) => {
                    let mut tool = Tool::default();
                    tool.name = "echo".into();
                    tool.description = Some("Echo".into());
                    tool.input_schema = Arc::new(serde_json::Map::new());
                    server.respond(
                        request.id,
                        ServerResult::ListToolsResult(ListToolsResult::with_all_items(vec![tool])),
                    );
                }
                ClientRequest::CallToolRequest(_) => {
                    for idx in 0..32 {
                        server.send(&logging(json!({"burst": idx})));
                    }
                    server.respond(
                        request.id,
                        ServerResult::CallToolResult(rmcp::model::CallToolResult::success(vec![
                            ContentBlock::text("noisy call completed"),
                        ])),
                    );
                }
                _ => {}
            }
        })
        .await;
        url
    }

    /// Counts how many times the session observer fired.
    fn counting_observer() -> (
        Arc<dyn Fn() + Send + Sync>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observer_calls = Arc::clone(&calls);
        let observer: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            observer_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        (observer, calls)
    }

    #[test]
    fn first_endpoint_of_a_connection_is_not_a_session_replacement() {
        let (endpoint_tx, _rx) = tokio::sync::watch::channel::<Option<Arc<str>>>(None);
        let (observer, calls) = counting_observer();

        publish_endpoint(&endpoint_tx, Arc::from("http://host/post"), Some(&observer));

        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert_eq!(
            endpoint_tx.borrow().as_deref(),
            Some("http://host/post"),
            "the endpoint is still published even though nothing was replaced"
        );
    }

    #[test]
    fn re_advertising_the_same_endpoint_is_treated_as_a_resume() {
        let (endpoint_tx, _rx) = tokio::sync::watch::channel::<Option<Arc<str>>>(None);
        let (observer, calls) = counting_observer();

        publish_endpoint(&endpoint_tx, Arc::from("http://host/post"), Some(&observer));
        publish_endpoint(&endpoint_tx, Arc::from("http://host/post"), Some(&observer));

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "an unchanged endpoint must not force a resubscribe"
        );
    }

    #[test]
    fn a_changed_endpoint_reports_a_replaced_session() {
        let (endpoint_tx, _rx) = tokio::sync::watch::channel::<Option<Arc<str>>>(None);
        let (observer, calls) = counting_observer();

        publish_endpoint(
            &endpoint_tx,
            Arc::from("http://host/post?s=1"),
            Some(&observer),
        );
        publish_endpoint(
            &endpoint_tx,
            Arc::from("http://host/post?s=2"),
            Some(&observer),
        );

        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(
            endpoint_tx.borrow().as_deref(),
            Some("http://host/post?s=2")
        );
    }

    #[test]
    fn endpoint_changes_are_published_without_an_observer() {
        let (endpoint_tx, _rx) = tokio::sync::watch::channel::<Option<Arc<str>>>(None);

        publish_endpoint(&endpoint_tx, Arc::from("http://host/a"), None);
        publish_endpoint(&endpoint_tx, Arc::from("http://host/b"), None);

        assert_eq!(endpoint_tx.borrow().as_deref(), Some("http://host/b"));
    }

    #[tokio::test]
    async fn a_redial_the_server_refuses_ends_the_stream_instead_of_redialing() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let opens = Arc::new(AtomicUsize::new(0));
        let router = axum::Router::new().route(
            "/mcp",
            axum::routing::get({
                let opens = Arc::clone(&opens);
                move || {
                    let first = opens.fetch_add(1, Ordering::SeqCst) == 0;
                    async move {
                        let response = axum::response::Response::builder();
                        if first {
                            // The stream ends at once, so the transport redials.
                            response
                                .header(header::CONTENT_TYPE, "text/event-stream")
                                .body(axum::body::Body::from(
                                    "event: endpoint\ndata: /messages\n\n",
                                ))
                        } else {
                            response
                                .status(StatusCode::UNAUTHORIZED)
                                .body(axum::body::Body::empty())
                        }
                        .unwrap()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let uri: Arc<str> = format!("http://127.0.0.1:{port}/mcp").into();
        let client = crate::upstream_http::client(crate::upstream_http::Redirects::Never).unwrap();
        let token: Option<Arc<str>> = Some("token".into());
        let first = open_sse_stream(client.clone(), uri.clone(), token.clone(), None)
            .await
            .unwrap();
        let (endpoint_tx, _endpoint_rx) = tokio::sync::watch::channel(None);
        let (response_tx, _response_rx) = tokio::sync::mpsc::channel(CHANNEL_CAPACITY);
        let (notification_tx, _notification_rx) = tokio::sync::mpsc::channel(CHANNEL_CAPACITY);
        let mut config = LegacySseTransportConfig::with_uri(uri);
        config.auth_token = token;
        let quit = tokio::time::timeout(
            Duration::from_secs(10),
            run_sse_loop(
                client,
                config,
                first,
                endpoint_tx,
                response_tx,
                notification_tx,
                tokio_util::sync::CancellationToken::new(),
            ),
        )
        .await
        .expect("a refused redial ends the loop");

        match quit {
            Err(WorkerQuitReason::Fatal { error, .. }) => {
                assert!(error.refuses_credentials(), "{error}");
            }
            other => panic!("expected the refusal to end the stream, got {other:?}"),
        }
        assert_eq!(opens.load(Ordering::SeqCst), 2);
    }
}
