mod stateful;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub(crate) use stateful::SessionEnd;
pub use stateful::SessionStore;

/// What a session store call can fail with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    #[error("session not found")]
    NotFound,
    #[error("too many sessions")]
    TooMany,
}

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

/// Whether a session's client still has the access it opened the session with.
pub type AccessCheck = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

/// Read-only snapshot of a downstream HTTP session for operator inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownstreamSessionSnapshot {
    pub session_id: String,
    pub client_type: crate::types::ClientType,
    /// The OAuth client whose token opened the session, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<String>,
    /// Seconds since the session was created.
    pub connected_seconds: u64,
    /// Seconds since the session was last active.
    pub idle_seconds: u64,
    /// Configured inactivity timeout in seconds.
    pub timeout_seconds: u64,
}

impl From<DownstreamSessionSnapshot> for crate::ipc::IpcLiveSessionInfo {
    fn from(snapshot: DownstreamSessionSnapshot) -> Self {
        Self {
            transport: crate::ipc::LiveSessionTransport::Http,
            client_id: None,
            link_target: None,
            session_id: snapshot.session_id,
            client_type: snapshot.client_type,
            client_info: None,
            adapter_version: None,
            host: None,
            client_key: crate::downstream_identity::remote_client_key(
                snapshot.grant.as_deref(),
                snapshot.client_type,
            ),
            access_key: Some(crate::downstream_identity::remote_access_key(
                snapshot.grant.as_deref(),
            )),
            target: crate::downstream_identity::client_target(None, snapshot.client_type),
            grant: snapshot.grant,
            connected_secs: snapshot.connected_seconds,
            last_activity_secs: Some(snapshot.idle_seconds),
        }
    }
}

/// Every live downstream session: the daemon's local connectors and the HTTP
/// sessions in `http`, local first, then by client type and session id.
pub fn live_sessions(
    local: Vec<crate::ipc::IpcLiveSessionInfo>,
    http: &SessionStore,
) -> Vec<crate::ipc::IpcLiveSessionInfo> {
    use crate::ipc::LiveSessionTransport;
    let transport_order = |transport: LiveSessionTransport| match transport {
        LiveSessionTransport::DaemonProxy => 0,
        LiveSessionTransport::Http => 1,
    };
    let mut sessions = local;
    sessions.extend(http.snapshots().into_iter().map(Into::into));
    sessions.sort_by(|a, b| {
        transport_order(a.transport)
            .cmp(&transport_order(b.transport))
            .then(a.client_type.to_string().cmp(&b.client_type.to_string()))
            .then(a.session_id.cmp(&b.session_id))
    });
    sessions
}
