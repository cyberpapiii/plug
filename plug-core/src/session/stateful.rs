use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::http::error::HttpError;

use super::{SessionStore, SseMessage};

type SessionRemoveHook = Arc<dyn Fn(&str) + Send + Sync>;

/// Current in-memory stateful downstream session store.
pub struct StatefulSessionStore {
    sessions: Arc<DashMap<String, SessionState>>,
    max_sessions: usize,
    timeout: Duration,
    on_remove: RwLock<Option<SessionRemoveHook>>,
}

struct SessionState {
    last_activity: Instant,
    sse_sender: Option<mpsc::Sender<SseMessage>>,
    pending_notifications: VecDeque<SseMessage>,
    client_type: crate::types::ClientType,
}

impl StatefulSessionStore {
    pub fn new(timeout_secs: u64, max_sessions: usize) -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            max_sessions,
            timeout: Duration::from_secs(timeout_secs),
            on_remove: RwLock::new(None),
        }
    }

    pub fn set_remove_hook(&self, hook: SessionRemoveHook) {
        if let Ok(mut guard) = self.on_remove.write() {
            *guard = Some(hook);
        }
    }

    fn notify_removed(&self, session_id: &str) {
        if let Ok(guard) = self.on_remove.read() {
            if let Some(hook) = guard.as_ref() {
                hook(session_id);
            }
        }
    }

    fn try_send_to_session(&self, session_id: &str, message: &SseMessage) {
        let mut remove_session = false;
        let mut clear_sender = false;

        if let Some(entry) = self.sessions.get(session_id) {
            if entry.last_activity.elapsed() > self.timeout {
                remove_session = true;
            } else if let Some(sender) = entry.sse_sender.as_ref() {
                match sender.try_send(message.clone()) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        clear_sender = true;
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        tracing::warn!(
                            session_id = %session_id,
                            "dropping slow SSE client from targeted notification delivery"
                        );
                        clear_sender = true;
                    }
                }
            }
        } else {
            return;
        }

        if remove_session {
            self.sessions.remove(session_id);
            self.notify_removed(session_id);
            return;
        }

        if clear_sender {
            if let Some(mut entry) = self.sessions.get_mut(session_id) {
                entry.sse_sender = None;
            }
        } else if let Some(mut entry) = self.sessions.get_mut(session_id) {
            if entry.sse_sender.is_none() {
                const PENDING_LIMIT: usize = 32;
                if entry.pending_notifications.len() >= PENDING_LIMIT {
                    entry.pending_notifications.pop_front();
                }
                entry.pending_notifications.push_back(message.clone());
            }
        }
    }
}

impl SessionStore for StatefulSessionStore {
    fn create_session(&self) -> Result<String, HttpError> {
        if self.sessions.len() >= self.max_sessions {
            return Err(HttpError::TooManySessions);
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        self.sessions.insert(
            session_id.clone(),
            SessionState {
                last_activity: Instant::now(),
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
            self.notify_removed(session_id);
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
        while let Some(message) = entry.pending_notifications.pop_front() {
            if let Some(active_sender) = entry.sse_sender.as_ref() {
                match active_sender.try_send(message) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_))
                    | Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
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
            self.notify_removed(session_id);
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
        let on_remove = self.on_remove.read().ok().and_then(|guard| guard.clone());

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
                        let expired_ids = sessions
                            .iter()
                            .filter(|entry| entry.last_activity.elapsed() > timeout)
                            .map(|entry| entry.key().clone())
                            .collect::<Vec<_>>();
                        for session_id in &expired_ids {
                            sessions.remove(session_id);
                        }
                        let expired = expired_ids.len();
                        for session_id in expired_ids {
                            if let Some(hook) = on_remove.as_ref() {
                                hook(&session_id);
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

    #[test]
    fn session_count() {
        let store = StatefulSessionStore::new(1800, 100);
        assert_eq!(store.session_count(), 0);
        let id = store.create_session().unwrap();
        assert_eq!(store.session_count(), 1);
        store.remove(&id);
        assert_eq!(store.session_count(), 0);
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
}
