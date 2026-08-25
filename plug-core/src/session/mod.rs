mod stateful;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::http::error::HttpError;

pub use stateful::StatefulSessionStore;

/// Server-to-client notification payload queued or delivered via SSE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseMessage {
    serialized: Arc<str>,
    replay_key: Option<SseReplayKey>,
}

/// Optional key used to remove replay-buffer events whose lifecycle is shorter
/// than the session, such as reverse requests once a response arrives.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SseReplayKey {
    ReverseRequest(i64),
}

/// Session-owned SSE event with a stable per-session event id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub id: u64,
    pub message: SseMessage,
}

impl SseMessage {
    pub fn from_json_value(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::to_string(&value).map(|serialized| Self {
            serialized: Arc::from(serialized),
            replay_key: None,
        })
    }

    pub fn from_serialized(serialized: Arc<str>) -> Self {
        Self {
            serialized,
            replay_key: None,
        }
    }

    pub fn from_json_value_with_replay_key(
        value: serde_json::Value,
        replay_key: SseReplayKey,
    ) -> Result<Self, serde_json::Error> {
        serde_json::to_string(&value).map(|serialized| Self {
            serialized: Arc::from(serialized),
            replay_key: Some(replay_key),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.serialized
    }

    pub fn replay_key(&self) -> Option<&SseReplayKey> {
        self.replay_key.as_ref()
    }

    #[cfg(test)]
    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::from_str(self.as_str()).expect("valid serialized SSE payload")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSendOutcome {
    Delivered,
    Queued,
    SessionNotFound,
}

/// Which class of broadcast a notification belongs to.
///
/// Targeted notifications (progress, cancellation, resource updates) are not
/// listed: those answer a request the session already made, so the gate that
/// admitted the request has already authorized the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastKind {
    ToolList,
    ResourceList,
    PromptList,
    /// Log output: upstream `notifications/message`, plug's own auth
    /// telemetry, and fan-out lag warnings. Upstream log lines carry whatever
    /// the upstream server chose to log, from servers the receiving principal
    /// may hold no scope for at all, so this is gated like any other family.
    Logging,
}

/// Which broadcast notifications a session's principal may observe.
///
/// Computed once at `initialize` from that request's `DownstreamCallContext`,
/// because initialize is the only point where the whole policy input (era,
/// transport, principal, scopes, local trust) is in hand; the fan-out task is
/// shared by every session and has none of it.
///
/// The default denies everything, and deliberately so. `create_session` makes a
/// session visible to the fan-out task — which runs on another worker thread —
/// before `initialize` can record the audience. A notification arriving inside
/// that window would be buffered by `enqueue_pending` and replayed once the SSE
/// stream opens, so an admit-by-default would hand it to a client that never
/// qualified for it. Denying by default makes that window silent instead of
/// leaky. A loopback listener with no scopes to narrow gets
/// [`BroadcastAudience::unrestricted`] the moment initialize resolves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BroadcastAudience {
    pub tools: bool,
    pub resources: bool,
    pub prompts: bool,
    pub logging: bool,
}

impl BroadcastAudience {
    /// Every broadcast admitted — what an unscoped or fully-granted principal
    /// resolves to. Never a default; always the result of an explicit decision.
    pub fn unrestricted() -> Self {
        Self {
            tools: true,
            resources: true,
            prompts: true,
            logging: true,
        }
    }

    pub fn admits(&self, kind: BroadcastKind) -> bool {
        match kind {
            BroadcastKind::ToolList => self.tools,
            BroadcastKind::ResourceList => self.resources,
            BroadcastKind::PromptList => self.prompts,
            BroadcastKind::Logging => self.logging,
        }
    }
}

/// Transport type for a downstream client session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownstreamTransport {
    Http,
    Sse,
}

impl std::fmt::Display for DownstreamTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http => write!(f, "http"),
            Self::Sse => write!(f, "sse"),
        }
    }
}

/// Read-only snapshot of a downstream session for operator inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownstreamSessionSnapshot {
    pub session_id: String,
    pub transport: DownstreamTransport,
    pub client_type: crate::types::ClientType,
    /// Seconds since the session was created.
    pub connected_seconds: u64,
    /// Seconds since the session was last active.
    pub idle_seconds: u64,
    /// Configured inactivity timeout in seconds.
    pub timeout_seconds: u64,
}

/// Trait boundary for downstream session storage.
///
/// `plug` currently uses a stateful in-memory implementation, but this trait
/// marks the seam where a stateless or external-backed implementation would fit.
pub trait SessionStore: Send + Sync {
    fn create_session(&self) -> Result<String, HttpError>;
    fn validate(&self, session_id: &str) -> Result<(), HttpError>;
    fn touch(&self, session_id: &str) -> Result<(), HttpError>;
    fn has_live_sse_sender(&self, session_id: &str) -> Result<bool, HttpError>;
    fn set_sse_sender(
        &self,
        session_id: &str,
        sender: mpsc::Sender<SseEvent>,
        last_event_id: Option<u64>,
    ) -> Result<(), HttpError>;
    fn set_client_type(
        &self,
        session_id: &str,
        client_type: crate::types::ClientType,
    ) -> Result<(), HttpError>;
    fn get_client_type(&self, session_id: &str) -> Result<crate::types::ClientType, HttpError>;
    fn set_broadcast_audience(
        &self,
        session_id: &str,
        audience: BroadcastAudience,
    ) -> Result<(), HttpError>;
    fn remove(&self, session_id: &str) -> bool;
    fn broadcast(&self, message: SseMessage, kind: BroadcastKind);
    fn send_to_session(&self, session_id: &str, message: SseMessage);
    fn send_to_live_session(&self, session_id: &str, message: SseMessage) -> SessionSendOutcome;
    fn remove_replay_events_by_key(&self, session_id: &str, key: &SseReplayKey);
    fn spawn_cleanup_task(&self, cancel: CancellationToken);
    fn session_count(&self) -> usize;

    /// Return a read-only snapshot of currently tracked HTTP sessions.
    fn session_snapshots(&self) -> Vec<DownstreamSessionSnapshot>;
}
