use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::http::error::HttpError;

use super::{DownstreamSessionSnapshot, DownstreamTransport, SessionStore, SseMessage};

/// Current in-memory stateful downstream session store.
pub struct StatefulSessionStore {
    sessions: Arc<DashMap<String, SessionState>>,
    admission_lock: std::sync::Mutex<()>,
    max_sessions: usize,
    timeout: Duration,
    /// Optional channel to notify when sessions are implicitly removed (expiry).
    /// The receiver should clean up any subscription state for the expired session.
    expiry_tx: Option<mpsc::UnboundedSender<String>>,
}

struct SessionState {
    last_activity: Instant,
    created_at: Instant,
    sse_sender: Option<mpsc::Sender<SseMessage>>,
    pending_notifications: VecDeque<SseMessage>,
    client_type: crate::types::ClientType,
}

impl StatefulSessionStore {
    /// Return a transport-aware snapshot of tracked HTTP sessions.
    pub fn list_sessions(&self) -> Vec<DownstreamSessionSnapshot> {
        let timeout = self.timeout;

        let mut snapshots: Vec<DownstreamSessionSnapshot> = self
            .sessions
            .iter()
            .filter_map(|entry| {
                let state = entry.value();
                if state.last_activity.elapsed() > timeout {
                    return None;
                }

                Some(DownstreamSessionSnapshot {
                    session_id: entry.key().clone(),
                    transport: DownstreamTransport::Http,
                    client_type: state.client_type,
                    connected_seconds: state.created_at.elapsed().as_secs(),
                    idle_seconds: state.last_activity.elapsed().as_secs(),
                    timeout_seconds: self.timeout.as_secs(),
                })
            })
            .collect();

        snapshots.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        snapshots
    }

    pub fn new(timeout_secs: u64, max_sessions: usize) -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            admission_lock: std::sync::Mutex::new(()),
            max_sessions,
            timeout: Duration::from_secs(timeout_secs),
            expiry_tx: None,
        }
    }

    /// Set a channel that receives session IDs when sessions are implicitly removed
    /// (timeout expiry). This allows callers to clean up external state like
    /// resource subscriptions.
    pub fn with_expiry_notifier(mut self, tx: mpsc::UnboundedSender<String>) -> Self {
        self.expiry_tx = Some(tx);
        self
    }

    fn notify_expired(&self, session_id: &str) {
        if let Some(tx) = &self.expiry_tx {
            let _ = tx.send(session_id.to_owned());
        }
    }

    fn prune_expired_sessions(&self) {
        let expired_ids: Vec<String> = self
            .sessions
            .iter()
            .filter(|entry| entry.last_activity.elapsed() > self.timeout)
            .map(|entry| entry.key().clone())
            .collect();
        for session_id in expired_ids {
            if self.sessions.remove(&session_id).is_some() {
                self.notify_expired(&session_id);
            }
        }
    }

    fn enqueue_pending(entry: &mut SessionState, message: SseMessage) {
        const PENDING_LIMIT: usize = 32;
        if entry.pending_notifications.len() >= PENDING_LIMIT {
            entry.pending_notifications.pop_front();
        }
        entry.pending_notifications.push_back(message);
    }

    fn try_send_to_session(&self, session_id: &str, message: &SseMessage) {
        let mut remove_session = false;
        let mut clear_sender = false;
        let mut queue_message = false;
        let mut delivered = false;

        if let Some(entry) = self.sessions.get(session_id) {
            if entry.last_activity.elapsed() > self.timeout {
                remove_session = true;
            } else if let Some(sender) = entry.sse_sender.as_ref() {
                match sender.try_send(message.clone()) {
                    Ok(()) => delivered = true,
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        clear_sender = true;
                        queue_message = true;
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        tracing::warn!(
                            session_id = %session_id,
                            "dropping slow SSE client from targeted notification delivery"
                        );
                        clear_sender = true;
                        queue_message = true;
                    }
                }
            }
        } else {
            return;
        }

        if remove_session {
            self.sessions.remove(session_id);
            self.notify_expired(session_id);
            return;
        }

        if clear_sender {
            if let Some(mut entry) = self.sessions.get_mut(session_id) {
                entry.sse_sender = None;
                if queue_message {
                    Self::enqueue_pending(&mut entry, message.clone());
                }
            }
        } else if let Some(mut entry) = self.sessions.get_mut(session_id) {
            if delivered {
                entry.last_activity = Instant::now();
            } else if entry.sse_sender.is_none() {
                Self::enqueue_pending(&mut entry, message.clone());
            }
        }
    }
}

impl SessionStore for StatefulSessionStore {
    fn create_session(&self) -> Result<String, HttpError> {
        let _guard = self.admission_lock.lock().expect("session admission mutex poisoned");
        self.prune_expired_sessions();
        if self.sessions.len() >= self.max_sessions {
            return Err(HttpError::TooManySessions);
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        let now = Instant::now();
        self.sessions.insert(
            session_id.clone(),
            SessionState {
                last_activity: now,
                created_at: now,
                sse_sender: None,
                pending_notifications: VecDeque::new(),
                client_type: crate::types::ClientType::Unknown,
            },
        );

        tracing::debug!(session_id = %session_id, "session created");
        Ok(session_id)
    }

    fn validate(&self, session_id: &str) -> Result<(), HttpError> {
        let mut entry = self
            .sessions
            .get_mut(session_id)
            .ok_or(HttpError::SessionNotFound)?;

        if entry.last_activity.elapsed() > self.timeout {
            drop(entry);
            self.sessions.remove(session_id);
            self.notify_expired(session_id);
            return Err(HttpError::SessionNotFound);
        }

        entry.last_activity = Instant::now();
        Ok(())
    }

    fn set_sse_sender(
        &self,
        session_id: &str,
        sender: mpsc::Sender<SseMessage>,
    ) -> Result<(), HttpError> {
        let mut entry = self
            .sessions
            .get_mut(session_id)
            .ok_or(HttpError::SessionNotFound)?;
        entry.sse_sender = Some(sender);
        entry.last_activity = Instant::now();
        while let Some(message) = entry.pending_notifications.pop_front() {
            if let Some(active_sender) = entry.sse_sender.as_ref() {
                match active_sender.try_send(message) {
                    Ok(()) => entry.last_activity = Instant::now(),
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(message))
                    | Err(tokio::sync::mpsc::error::TrySendError::Full(message)) => {
                        Self::enqueue_pending(&mut entry, message);
                        entry.sse_sender = None;
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    fn set_client_type(
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

    fn get_client_type(&self, session_id: &str) -> Result<crate::types::ClientType, HttpError> {
        let entry = self
            .sessions
            .get(session_id)
            .ok_or(HttpError::SessionNotFound)?;
        Ok(entry.client_type)
    }

    fn remove(&self, session_id: &str) -> bool {
        let removed = self.sessions.remove(session_id).is_some();
        if removed {
            tracing::debug!(session_id = %session_id, "session removed");
        }
        removed
    }

    fn broadcast(&self, message: SseMessage) {
        let session_ids: Vec<String> = self
            .sessions
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        for session_id in session_ids {
            self.try_send_to_session(&session_id, &message);
        }
    }

    fn send_to_session(&self, session_id: &str, message: SseMessage) {
        self.try_send_to_session(session_id, &message);
    }

    fn spawn_cleanup_task(&self, cancel: CancellationToken) {
        let sessions = Arc::clone(&self.sessions);
        let timeout = self.timeout;
        let expiry_tx = self.expiry_tx.clone();

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
                        // Collect expired session IDs before retain removes them,
                        // so we can notify the subscription cleanup listener.
                        let expired_ids: Vec<String> = if expiry_tx.is_some() {
                            sessions.iter()
                                .filter(|entry| entry.last_activity.elapsed() > timeout)
                                .map(|entry| entry.key().clone())
                                .collect()
                        } else {
                            Vec::new()
                        };

                        let before = sessions.len();
                        sessions.retain(|_, state| state.last_activity.elapsed() <= timeout);
                        let expired = before - sessions.len();

                        if let Some(tx) = &expiry_tx {
                            for session_id in expired_ids {
                                let _ = tx.send(session_id);
                            }
                        }

                        if expired > 0 {
                            tracing::info!(expired, remaining = sessions.len(), "cleaned up expired sessions");
                        }
                    }
                }
            }
        });
    }

    fn session_count(&self) -> usize {
        self.sessions.len()
    }

    fn session_snapshots(&self) -> Vec<DownstreamSessionSnapshot> {
        self.list_sessions()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_session_returns_uuid() {
        let store = StatefulSessionStore::new(1800, 100);
        let id = store.create_session().unwrap();
        assert!(!id.is_empty());
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }

    #[test]
    fn validate_existing_session() {
        let store = StatefulSessionStore::new(1800, 100);
        let id = store.create_session().unwrap();
        assert!(store.validate(&id).is_ok());
    }

    #[test]
    fn validate_nonexistent_session() {
        let store = StatefulSessionStore::new(1800, 100);
        assert!(store.validate("nonexistent").is_err());
    }

    #[test]
    fn remove_session() {
        let store = StatefulSessionStore::new(1800, 100);
        let id = store.create_session().unwrap();
        assert!(store.remove(&id));
        assert!(store.validate(&id).is_err());
    }

    #[test]
    fn max_sessions_cap() {
        let store = StatefulSessionStore::new(1800, 2);
        store.create_session().unwrap();
        store.create_session().unwrap();
        assert!(store.create_session().is_err());
    }

    #[tokio::test]
    async fn create_session_prunes_expired_sessions_before_enforcing_cap() {
        let store = StatefulSessionStore::new(0, 1);
        store.create_session().unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(store.create_session().is_ok());
    }

    #[test]
    fn session_count() {
        let store = StatefulSessionStore::new(1800, 100);
        assert_eq!(store.session_count(), 0);
        let id = store.create_session().unwrap();
        assert_eq!(store.session_count(), 1);
        store.remove(&id);
        assert_eq!(store.session_count(), 0);
    }

    #[test]
    fn list_sessions_reports_transport_and_client_type() {
        let store = StatefulSessionStore::new(1800, 100);
        let id = store.create_session().unwrap();
        store
            .set_client_type(&id, crate::types::ClientType::Cursor)
            .unwrap();

        let snapshots = store.list_sessions();
        assert_eq!(snapshots.len(), 1);

        let snapshot = &snapshots[0];
        assert_eq!(snapshot.session_id, id);
        assert_eq!(snapshot.transport, DownstreamTransport::Http);
        assert_eq!(snapshot.client_type, crate::types::ClientType::Cursor);
        assert_eq!(snapshot.timeout_seconds, 1800);
        assert!(snapshot.connected_seconds <= 1);
        assert!(snapshot.idle_seconds <= 1);
    }

    #[test]
    fn list_sessions_filters_expired_sessions() {
        let store = StatefulSessionStore::new(0, 100);
        let id = store.create_session().unwrap();

        std::thread::sleep(Duration::from_millis(10));

        assert!(
            store
                .list_sessions()
                .iter()
                .all(|snapshot| snapshot.session_id != id)
        );
        assert_eq!(store.session_count(), 1);
    }

    #[tokio::test]
    async fn expired_session_fails_validation() {
        let store = StatefulSessionStore::new(0, 100);
        let id = store.create_session().unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(store.validate(&id).is_err());
    }

    #[tokio::test]
    async fn broadcast_prunes_expired_sessions_before_delivery() {
        let store = StatefulSessionStore::new(0, 100);
        let id = store.create_session().unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        store.set_sse_sender(&id, tx).unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;
        store.broadcast(serde_json::json!({"type": "test"}));

        assert!(store.validate(&id).is_err());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn broadcast_skips_full_senders_without_blocking_other_sessions() {
        let store = StatefulSessionStore::new(1800, 100);
        let slow_id = store.create_session().unwrap();
        let fast_id = store.create_session().unwrap();

        let (slow_tx, _slow_rx) = mpsc::channel(1);
        let (fast_tx, mut fast_rx) = mpsc::channel(1);
        store.set_sse_sender(&slow_id, slow_tx.clone()).unwrap();
        store.set_sse_sender(&fast_id, fast_tx).unwrap();

        slow_tx
            .try_send(serde_json::json!({"type": "already-buffered"}))
            .unwrap();

        store.broadcast(serde_json::json!({"type": "broadcast"}));

        let received = tokio::time::timeout(Duration::from_secs(1), fast_rx.recv())
            .await
            .expect("fast receiver should not be blocked")
            .expect("fast receiver message present");
        assert_eq!(received["type"], "broadcast");
    }

    #[tokio::test]
    async fn targeted_message_is_requeued_when_sender_is_full() {
        let store = StatefulSessionStore::new(1800, 100);
        let id = store.create_session().unwrap();

        let (tx, _rx) = mpsc::channel(1);
        store.set_sse_sender(&id, tx.clone()).unwrap();
        tx.try_send(serde_json::json!({"type": "already-buffered"}))
            .unwrap();

        store.send_to_session(&id, serde_json::json!({"type": "requeued"}));

        let (new_tx, mut new_rx) = mpsc::channel(1);
        store.set_sse_sender(&id, new_tx).unwrap();

        let received = tokio::time::timeout(Duration::from_secs(1), new_rx.recv())
            .await
            .expect("requeued message should be delivered")
            .expect("requeued message present");
        assert_eq!(received["type"], "requeued");
    }
}
