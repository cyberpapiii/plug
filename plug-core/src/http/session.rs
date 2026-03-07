use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::error::HttpError;

/// Minimal session manager using DashMap for concurrent access.
///
/// Each session tracks its last activity time for expiry. SSE senders
/// are tracked per-session for server-initiated notifications.
pub struct SessionManager {
    /// session_id → SessionState
    sessions: Arc<DashMap<String, SessionState>>,
    max_sessions: usize,
    timeout: Duration,
}

struct SessionState {
    last_activity: Instant,
    /// SSE sender for this session (at most one active SSE stream per session).
    sse_sender: Option<mpsc::Sender<SseMessage>>,
    client_type: crate::types::ClientType,
}

/// A message to send via SSE to a client.
pub type SseMessage = serde_json::Value;

impl SessionManager {
    pub fn new(timeout_secs: u64, max_sessions: usize) -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            max_sessions,
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    /// Create a new session. Returns the session ID or error if at capacity.
    pub fn create_session(&self) -> Result<String, HttpError> {
        if self.sessions.len() >= self.max_sessions {
            return Err(HttpError::TooManySessions);
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        self.sessions.insert(
            session_id.clone(),
            SessionState {
                last_activity: Instant::now(),
                sse_sender: None,
                client_type: crate::types::ClientType::Unknown,
            },
        );

        tracing::debug!(session_id = %session_id, "session created");
        Ok(session_id)
    }

    /// Validate a session exists and is not expired. Updates last_activity.
    pub fn validate(&self, session_id: &str) -> Result<(), HttpError> {
        let mut entry = self
            .sessions
            .get_mut(session_id)
            .ok_or(HttpError::SessionNotFound)?;

        if entry.last_activity.elapsed() > self.timeout {
            drop(entry);
            self.sessions.remove(session_id);
            return Err(HttpError::SessionNotFound);
        }

        entry.last_activity = Instant::now();
        Ok(())
    }

    /// Register an SSE sender for a session.
    pub fn set_sse_sender(
        &self,
        session_id: &str,
        sender: mpsc::Sender<SseMessage>,
    ) -> Result<(), HttpError> {
        let mut entry = self
            .sessions
            .get_mut(session_id)
            .ok_or(HttpError::SessionNotFound)?;
        entry.sse_sender = Some(sender);
        Ok(())
    }

    /// Set the client type for a session (called during initialize).
    pub fn set_client_type(
        &self,
        session_id: &str,
        client_type: crate::types::ClientType,
    ) -> Result<(), HttpError> {
        let mut entry = self
            .sessions
            .get_mut(session_id)
            .ok_or(HttpError::SessionNotFound)?;
        entry.client_type = client_type;
        Ok(())
    }

    /// Get the client type for a session.
    pub fn get_client_type(&self, session_id: &str) -> Result<crate::types::ClientType, HttpError> {
        let entry = self
            .sessions
            .get(session_id)
            .ok_or(HttpError::SessionNotFound)?;
        Ok(entry.client_type)
    }

    /// Remove a session and clean up its resources.
    pub fn remove(&self, session_id: &str) -> bool {
        let removed = self.sessions.remove(session_id).is_some();
        if removed {
            tracing::debug!(session_id = %session_id, "session removed");
        }
        removed
    }

    /// Broadcast a notification to every session with an active SSE sender.
    ///
    /// Dead senders are cleared lazily so future fan-out skips them.
    pub fn broadcast(&self, message: SseMessage) {
        let mut expired_sessions = Vec::new();
        let senders: Vec<(String, mpsc::Sender<SseMessage>)> = self
            .sessions
            .iter()
            .filter_map(|entry| {
                if entry.last_activity.elapsed() > self.timeout {
                    expired_sessions.push(entry.key().clone());
                    return None;
                }

                entry
                    .sse_sender
                    .as_ref()
                    .map(|sender| (entry.key().clone(), sender.clone()))
            })
            .collect();

        let mut stale_sessions = Vec::new();
        for (session_id, sender) in senders {
            match sender.try_send(message.clone()) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    stale_sessions.push(session_id);
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!(
                        session_id = %session_id,
                        "dropping slow SSE client from notification fan-out"
                    );
                    stale_sessions.push(session_id);
                }
            }
        }

        for session_id in expired_sessions {
            self.sessions.remove(&session_id);
        }

        for session_id in stale_sessions {
            if let Some(mut entry) = self.sessions.get_mut(&session_id) {
                entry.sse_sender = None;
            }
        }
    }

    /// Spawn a background task that periodically cleans up expired sessions.
    pub fn spawn_cleanup_task(&self, cancel: CancellationToken) {
        let sessions = Arc::clone(&self.sessions);
        let timeout = self.timeout;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        tracing::debug!("session cleanup task shutting down");
                        break;
                    }
                    _ = interval.tick() => {
                        let before = sessions.len();
                        sessions.retain(|_, state| state.last_activity.elapsed() <= timeout);
                        let expired = before - sessions.len();
                        if expired > 0 {
                            tracing::info!(expired, remaining = sessions.len(), "cleaned up expired sessions");
                        }
                    }
                }
            }
        });
    }

    /// Get the number of active sessions.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[test]
    fn create_session_returns_uuid() {
        let mgr = SessionManager::new(1800, 100);
        let id = mgr.create_session().unwrap();
        assert!(!id.is_empty());
        // Should be valid UUID format
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }

    #[test]
    fn validate_existing_session() {
        let mgr = SessionManager::new(1800, 100);
        let id = mgr.create_session().unwrap();
        assert!(mgr.validate(&id).is_ok());
    }

    #[test]
    fn validate_nonexistent_session() {
        let mgr = SessionManager::new(1800, 100);
        assert!(mgr.validate("nonexistent").is_err());
    }

    #[test]
    fn remove_session() {
        let mgr = SessionManager::new(1800, 100);
        let id = mgr.create_session().unwrap();
        assert!(mgr.remove(&id));
        assert!(mgr.validate(&id).is_err());
    }

    #[test]
    fn max_sessions_cap() {
        let mgr = SessionManager::new(1800, 2);
        mgr.create_session().unwrap();
        mgr.create_session().unwrap();
        let result = mgr.create_session();
        assert!(result.is_err());
    }

    #[test]
    fn session_count() {
        let mgr = SessionManager::new(1800, 100);
        assert_eq!(mgr.session_count(), 0);
        let id = mgr.create_session().unwrap();
        assert_eq!(mgr.session_count(), 1);
        mgr.remove(&id);
        assert_eq!(mgr.session_count(), 0);
    }

    #[tokio::test]
    async fn expired_session_fails_validation() {
        let mgr = SessionManager::new(0, 100); // 0 second timeout = immediate expiry
        let id = mgr.create_session().unwrap();
        // Sleep briefly to ensure expiry
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(mgr.validate(&id).is_err());
    }

    #[tokio::test]
    async fn broadcast_prunes_expired_sessions_before_delivery() {
        let mgr = SessionManager::new(0, 100);
        let id = mgr.create_session().unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        mgr.set_sse_sender(&id, tx).unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;
        mgr.broadcast(serde_json::json!({"type": "test"}));

        assert!(mgr.validate(&id).is_err());
        assert!(rx.try_recv().is_err(), "expired session should not receive messages");
    }

    #[tokio::test]
    async fn broadcast_skips_full_senders_without_blocking_other_sessions() {
        let mgr = SessionManager::new(1800, 100);
        let slow_id = mgr.create_session().unwrap();
        let fast_id = mgr.create_session().unwrap();

        let (slow_tx, _slow_rx) = mpsc::channel(1);
        let (fast_tx, mut fast_rx) = mpsc::channel(1);
        mgr.set_sse_sender(&slow_id, slow_tx.clone()).unwrap();
        mgr.set_sse_sender(&fast_id, fast_tx).unwrap();

        slow_tx
            .try_send(serde_json::json!({"type": "already-buffered"}))
            .unwrap();

        mgr.broadcast(serde_json::json!({"type": "broadcast"}));

        let received = tokio::time::timeout(Duration::from_secs(1), fast_rx.recv())
            .await
            .expect("fast receiver should not be blocked")
            .expect("fast receiver message present");
        assert_eq!(received["type"], "broadcast");
    }
}
