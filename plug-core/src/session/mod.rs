mod stateful;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::http::error::HttpError;

pub use stateful::StatefulSessionStore;

/// Server-to-client notification payload queued or delivered via SSE.
pub type SseMessage = serde_json::Value;

/// Trait boundary for downstream session storage.
///
/// `plug` currently uses a stateful in-memory implementation, but this trait
/// marks the seam where a stateless or external-backed implementation would fit.
pub trait SessionStore: Send + Sync {
    fn create_session(&self) -> Result<String, HttpError>;
    fn validate(&self, session_id: &str) -> Result<(), HttpError>;
    fn set_sse_sender(
        &self,
        session_id: &str,
        sender: mpsc::Sender<SseMessage>,
    ) -> Result<(), HttpError>;
    fn set_client_type(
        &self,
        session_id: &str,
        client_type: crate::types::ClientType,
    ) -> Result<(), HttpError>;
    fn get_client_type(&self, session_id: &str) -> Result<crate::types::ClientType, HttpError>;
    fn remove(&self, session_id: &str) -> bool;
    fn broadcast(&self, message: SseMessage);
    fn send_to_session(&self, session_id: &str, message: SseMessage);
    fn spawn_cleanup_task(&self, cancel: CancellationToken);
    fn session_count(&self) -> usize;
}
