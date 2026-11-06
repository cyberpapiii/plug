//! MCP Events: remote clients subscribe to an event and Plug posts each one to
//! their webhook. `hub` is the subscription engine every source shares; a
//! source decides what its events are and when one happens. Plug has two:
//! watching a tool (`watch`) and Slack (`crate::slack_events`).
mod access;
mod delivery;
mod hub;
#[cfg(test)]
mod tests;
mod watch;

use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::{HeaderMap, StatusCode};
use serde_json::Value;

pub use crate::config::{
    EVENT_SCOPE, EventsConfig, MIN_WATCH_SECS, WatchConfig, events_missing, events_possible,
};
pub use access::RuntimeWatchAccess;
pub use delivery::{DeliveryResponse, EventDelivery, HttpsDelivery};
pub(crate) use delivery::{hmac_sha256, validate_url};
pub(crate) use hub::MAX_QUEUE;
pub use hub::{Access, Hub, Pending, Source, SourceState, State, Subscription};
pub use watch::{EventStatus, WatchAccess, WatchEvents, WatchHealth, WatchSource, WatchState};

pub(crate) const DAY: u64 = 86_400;
/// The largest event, and the largest body a source accepts from outside.
pub(crate) const MAX_BODY: usize = 256 * 1024;

/// What the HTTP transport needs from an event source. A new source is one
/// more entry in `HttpState`'s list, with no transport changes.
#[allow(
    clippy::double_must_use,
    reason = "async_trait generates redundant must_use on boxed futures"
)]
#[async_trait::async_trait]
pub trait EventSource: Send + Sync {
    /// Whether `client_id` may learn this source exists, so the events
    /// capability and its catalog are worth showing it.
    fn discoverable_to(&self, client_id: &str) -> bool;
    /// The event definitions `events/list` returns to `client_id`.
    fn catalog(&self, client_id: &str) -> Vec<Value>;
    /// Whether this source makes the event named `event`.
    fn owns(&self, event: &str) -> bool;
    async fn subscribe(&self, client_id: &str, params: &Value) -> Result<Value, EventError>;
    async fn unsubscribe(&self, client_id: &str, params: &Value) -> Result<Value, EventError>;
    /// The path where the source takes events pushed to it from outside.
    fn ingest_path(&self) -> Option<&'static str>;
    async fn ingest(&self, headers: &HeaderMap, body: &[u8]) -> Result<Value, EventError>;
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct EventError {
    pub code: i32,
    pub message: &'static str,
    pub reason: Option<&'static str>,
}

impl EventError {
    /// The status an ingest endpoint answers with.
    pub fn http_status(&self) -> StatusCode {
        match self.code {
            -32001 => StatusCode::UNAUTHORIZED,
            -32602 => StatusCode::BAD_REQUEST,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

pub(crate) fn invalid(message: &'static str) -> EventError {
    EventError {
        code: -32602,
        message,
        reason: None,
    }
}
pub(crate) fn denied() -> EventError {
    EventError {
        code: -32001,
        message: "event access denied",
        reason: None,
    }
}
pub(crate) fn unavailable() -> EventError {
    EventError {
        code: -32603,
        message: "event state unavailable",
        reason: None,
    }
}
pub(crate) fn callback_error(reason: &'static str) -> EventError {
    EventError {
        code: -32015,
        message: "callback verification failed",
        reason: Some(reason),
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn iso_time(seconds: u64) -> Result<String, EventError> {
    chrono::DateTime::from_timestamp(
        i64::try_from(seconds).map_err(|_| invalid("invalid timestamp"))?,
        0,
    )
    .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    .ok_or_else(|| invalid("invalid timestamp"))
}
