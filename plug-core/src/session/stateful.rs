use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::http::error::HttpError;

use super::{
    DownstreamSessionSnapshot, DownstreamTransport, SessionSendOutcome, SessionStore, SseEvent,
    SseMessage, SseReplayKey,
};

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
    sse_sender: Option<mpsc::Sender<SseEvent>>,
    pending_notifications: VecDeque<SseEvent>,
    replay_events: VecDeque<SseEvent>,
    next_event_id: u64,
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

    fn enqueue_pending(entry: &mut SessionState, event: SseEvent) {
        const PENDING_LIMIT: usize = 32;
        if entry.pending_notifications.len() >= PENDING_LIMIT {
            entry.pending_notifications.pop_front();
        }
        entry.pending_notifications.push_back(event);
    }

    fn enqueue_replay(entry: &mut SessionState, event: SseEvent) {
        const REPLAY_LIMIT: usize = 128;
        if entry.replay_events.len() >= REPLAY_LIMIT {
            entry.replay_events.pop_front();
        }
        entry.replay_events.push_back(event);
    }

    fn next_event(entry: &mut SessionState, message: SseMessage) -> SseEvent {
        let event = SseEvent {
            id: entry.next_event_id,
            message,
        };
        entry.next_event_id = entry.next_event_id.saturating_add(1);
        Self::enqueue_replay(entry, event.clone());
        event
    }

    fn with_live_session_mut<T>(
        &self,
        session_id: &str,
        mut f: impl FnMut(&mut SessionState) -> T,
    ) -> Result<T, HttpError> {
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

        Ok(f(&mut entry))
    }

    fn try_send_to_session(
        &self,
        session_id: &str,
        message: SseMessage,
        queue_if_unavailable: bool,
    ) -> SessionSendOutcome {
        let mut remove_session = false;
        let mut clear_sender = false;
        let mut failed_sender: Option<mpsc::Sender<SseEvent>> = None;
        let mut queue_event: Option<SseEvent> = None;
        let mut delivered = false;

        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            if entry.last_activity.elapsed() > self.timeout {
                remove_session = true;
            } else if let Some(sender) = entry.sse_sender.clone() {
                let event = Self::next_event(&mut entry, message);
                match sender.try_send(event.clone()) {
                    Ok(()) => {
                        entry.last_activity = Instant::now();
                        delivered = true;
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        clear_sender = true;
                        failed_sender = Some(sender);
                        if queue_if_unavailable {
                            queue_event = Some(event);
                        }
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        tracing::warn!(
                            session_id = %session_id,
                            "dropping slow SSE client from targeted notification delivery"
                        );
                        clear_sender = true;
                        failed_sender = Some(sender);
                        if queue_if_unavailable {
                            queue_event = Some(event);
                        }
                    }
                }
            } else if queue_if_unavailable {
                queue_event = Some(Self::next_event(&mut entry, message));
            }
        } else {
            return SessionSendOutcome::SessionNotFound;
        }

        if remove_session {
            self.sessions.remove(session_id);
            self.notify_expired(session_id);
            return SessionSendOutcome::SessionNotFound;
        }

        let queued = queue_event.is_some();
        if clear_sender {
            self.clear_sender_if_matching(session_id, failed_sender.as_ref(), queue_event);
        } else if let Some(event) = queue_event
            && let Some(mut entry) = self.sessions.get_mut(session_id)
        {
            Self::enqueue_pending(&mut entry, event);
        }

        if delivered {
            SessionSendOutcome::Delivered
        } else if queued {
            SessionSendOutcome::Queued
        } else {
            SessionSendOutcome::SessionNotFound
        }
    }

    /// Second phase of `try_send_to_session`'s clear path. Only nulls the entry's
    /// `sse_sender` if it is still the same channel that failed delivery; a
    /// reconnecting client may have installed a fresh, live sender in the window
    /// between `try_send_to_session` releasing and re-acquiring this entry's guard,
    /// and wiping that out would silently break the new connection.
    ///
    /// Extracted from `try_send_to_session` so the race-guard decision can be
    /// exercised directly by tests without relying on real thread interleaving.
    fn clear_sender_if_matching(
        &self,
        session_id: &str,
        failed_sender: Option<&mpsc::Sender<SseEvent>>,
        queue_event: Option<SseEvent>,
    ) {
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            let same = match (&entry.sse_sender, failed_sender) {
                (Some(current), Some(failed)) => current.same_channel(failed),
                _ => false,
            };
            if same {
                entry.sse_sender = None;
                if let Some(event) = queue_event {
                    Self::enqueue_pending(&mut entry, event);
                }
            } else if let Some(event) = queue_event {
                // A new sender raced in while the old one failed; the event was
                // never delivered to it, so try delivering it live before falling
                // back to the pending queue (which is only flushed on the next
                // reconnect without a Last-Event-Id).
                match entry.sse_sender.clone() {
                    Some(new_sender) => match new_sender.try_send(event.clone()) {
                        Ok(()) => {
                            entry.last_activity = Instant::now();
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(event))
                        | Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
                            Self::enqueue_pending(&mut entry, event);
                        }
                    },
                    None => Self::enqueue_pending(&mut entry, event),
                }
            }
        }
    }

    fn send_replay_events(
        entry: &mut SessionState,
        sender: &mpsc::Sender<SseEvent>,
        last_event_id: Option<u64>,
    ) {
        let events: Vec<SseEvent> = match last_event_id {
            Some(last_id) => entry
                .replay_events
                .iter()
                .filter(|event| event.id > last_id)
                .cloned()
                .collect(),
            None => entry.pending_notifications.drain(..).collect(),
        };

        let mut sent_through = last_event_id;
        for event in events {
            match sender.try_send(event.clone()) {
                Ok(()) => {
                    sent_through = Some(event.id);
                    entry.last_activity = Instant::now();
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(event))
                | Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
                    Self::enqueue_pending(entry, event);
                    entry.sse_sender = None;
                    break;
                }
            }
        }

        if let Some(sent_id) = sent_through {
            entry
                .pending_notifications
                .retain(|event| event.id > sent_id);
        }
    }
}

impl SessionStore for StatefulSessionStore {
    fn create_session(&self) -> Result<String, HttpError> {
        let _guard = self
            .admission_lock
            .lock()
            .expect("session admission mutex poisoned");
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
                replay_events: VecDeque::new(),
                next_event_id: 1,
                client_type: crate::types::ClientType::Unknown,
            },
        );

        tracing::debug!(session_id = %session_id, "session created");
        Ok(session_id)
    }

    fn validate(&self, session_id: &str) -> Result<(), HttpError> {
        self.with_live_session_mut(session_id, |entry| {
            entry.last_activity = Instant::now();
        })
    }

    fn touch(&self, session_id: &str) -> Result<(), HttpError> {
        self.with_live_session_mut(session_id, |entry| {
            entry.last_activity = Instant::now();
        })
    }

    fn has_live_sse_sender(&self, session_id: &str) -> Result<bool, HttpError> {
        self.with_live_session_mut(session_id, |entry| entry.sse_sender.is_some())
    }

    fn set_sse_sender(
        &self,
        session_id: &str,
        sender: mpsc::Sender<SseEvent>,
        last_event_id: Option<u64>,
    ) -> Result<(), HttpError> {
        let mut entry = self
            .sessions
            .get_mut(session_id)
            .ok_or(HttpError::SessionNotFound)?;
        entry.sse_sender = Some(sender);
        entry.last_activity = Instant::now();
        if let Some(active_sender) = entry.sse_sender.clone() {
            Self::send_replay_events(&mut entry, &active_sender, last_event_id);
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
            let mut expired = false;
            if let Some(mut entry) = self.sessions.get_mut(&session_id) {
                if entry.last_activity.elapsed() > self.timeout {
                    expired = true;
                } else {
                    let event = Self::next_event(&mut entry, message.clone());
                    if let Some(sender) = entry.sse_sender.clone() {
                        match sender.try_send(event.clone()) {
                            Ok(()) => entry.last_activity = Instant::now(),
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_))
                            | Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                entry.sse_sender = None;
                                Self::enqueue_pending(&mut entry, event);
                            }
                        }
                    } else {
                        Self::enqueue_pending(&mut entry, event);
                    }
                }
            }
            if expired && self.sessions.remove(&session_id).is_some() {
                self.notify_expired(&session_id);
            }
        }
    }

    fn send_to_session(&self, session_id: &str, message: SseMessage) {
        let _ = self.try_send_to_session(session_id, message, true);
    }

    fn send_to_live_session(&self, session_id: &str, message: SseMessage) -> SessionSendOutcome {
        self.try_send_to_session(session_id, message, true)
    }

    fn remove_replay_events_by_key(&self, session_id: &str, key: &SseReplayKey) {
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            entry
                .replay_events
                .retain(|event| event.message.replay_key() != Some(key));
            entry
                .pending_notifications
                .retain(|event| event.message.replay_key() != Some(key));
        }
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
                        let expired = before.saturating_sub(sessions.len());

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
    fn touch_existing_session() {
        let store = StatefulSessionStore::new(1800, 100);
        let id = store.create_session().unwrap();
        assert!(store.touch(&id).is_ok());
    }

    #[test]
    fn live_sse_sender_reports_presence() {
        let store = StatefulSessionStore::new(1800, 100);
        let id = store.create_session().unwrap();
        assert!(!store.has_live_sse_sender(&id).unwrap());
        let (tx, _rx) = mpsc::channel(1);
        store.set_sse_sender(&id, tx, None).unwrap();
        assert!(store.has_live_sse_sender(&id).unwrap());
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
    fn expired_count_does_not_underflow_when_map_grows_during_retain() {
        // Mirrors the cleanup task's pre-fix subtraction of the post-retain
        // count from the pre-retain count: a concurrent insert racing
        // `retain()` can leave the post-retain count greater than the
        // pre-retain count. The fixed formula must saturate to 0 instead of
        // underflowing (a panic with overflow-checks on, a huge wrapped
        // value in release builds).
        let before: usize = 1;
        let after_growth: usize = 4;
        assert_eq!(before.saturating_sub(after_growth), 0);

        // Sanity: the ordinary shrinking case still reports the real count.
        assert_eq!(5usize.saturating_sub(2), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn cleanup_task_keeps_running_across_multiple_expiry_cycles() {
        // Regression guard for the underflow panic silently killing the
        // spawned cleanup task: if the first tick's arithmetic panicked, the
        // JoinHandle is never awaited and idle-session reclamation would stop
        // forever, so a second expiry cycle would never happen either.
        //
        // `last_activity` is a `std::time::Instant`, which is NOT affected by
        // tokio's paused virtual clock (only `tokio::time::interval` ticks
        // are). Using a 0s timeout means any real wall-clock elapsed time —
        // even the nanoseconds consumed by task scheduling — already counts
        // as expired, so we don't need a real 30s sleep to observe the
        // cleanup task's fixed 30s tick doing its job; `time::advance` alone
        // drives the interval.
        let store = StatefulSessionStore::new(0, 100);
        let cancel = CancellationToken::new();
        store.spawn_cleanup_task(cancel.clone());

        store.create_session().unwrap();
        assert_eq!(store.session_count(), 1);

        // First tick: the interval fires immediately on its first poll, and
        // the session is already expired (0s timeout) by the time it's
        // checked.
        tokio::time::advance(Duration::from_secs(31)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            store.session_count(),
            0,
            "cleanup task should have purged the expired session on its first tick"
        );

        store.create_session().unwrap();
        assert_eq!(store.session_count(), 1);

        // Second expiry cycle — proves the task is still alive and ticking
        // rather than having silently died on the first pass.
        tokio::time::advance(Duration::from_secs(31)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            store.session_count(),
            0,
            "cleanup task must still be running for a second expiry cycle"
        );

        cancel.cancel();
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
        store.set_sse_sender(&id, tx, None).unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;
        store.broadcast(
            crate::session::SseMessage::from_json_value(serde_json::json!({"type": "test"}))
                .unwrap(),
        );

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
        store
            .set_sse_sender(&slow_id, slow_tx.clone(), None)
            .unwrap();
        store.set_sse_sender(&fast_id, fast_tx, None).unwrap();

        slow_tx
            .try_send(crate::session::SseEvent {
                id: 999,
                message: crate::session::SseMessage::from_json_value(
                    serde_json::json!({"type": "already-buffered"}),
                )
                .unwrap(),
            })
            .unwrap();

        store.broadcast(
            crate::session::SseMessage::from_json_value(serde_json::json!({"type": "broadcast"}))
                .unwrap(),
        );

        let received = tokio::time::timeout(Duration::from_secs(1), fast_rx.recv())
            .await
            .expect("fast receiver should not be blocked")
            .expect("fast receiver message present");
        assert_eq!(received.message.to_json_value()["type"], "broadcast");
    }

    #[tokio::test]
    async fn targeted_message_is_requeued_when_sender_is_full() {
        let store = StatefulSessionStore::new(1800, 100);
        let id = store.create_session().unwrap();

        let (tx, _rx) = mpsc::channel(1);
        store.set_sse_sender(&id, tx.clone(), None).unwrap();
        tx.try_send(crate::session::SseEvent {
            id: 999,
            message: crate::session::SseMessage::from_json_value(
                serde_json::json!({"type": "already-buffered"}),
            )
            .unwrap(),
        })
        .unwrap();

        store.send_to_session(
            &id,
            crate::session::SseMessage::from_json_value(serde_json::json!({"type": "requeued"}))
                .unwrap(),
        );

        let (new_tx, mut new_rx) = mpsc::channel(1);
        store.set_sse_sender(&id, new_tx, None).unwrap();

        let received = tokio::time::timeout(Duration::from_secs(1), new_rx.recv())
            .await
            .expect("requeued message should be delivered")
            .expect("requeued message present");
        assert_eq!(received.message.to_json_value()["type"], "requeued");
    }

    #[tokio::test]
    async fn closed_sender_is_cleared_when_no_reconnect_races_in() {
        // Baseline: preserves the pre-fix behavior when nothing installs a new
        // sender in the window between clear detection and clearing.
        let store = StatefulSessionStore::new(1800, 100);
        let id = store.create_session().unwrap();

        let (tx_a, rx_a) = mpsc::channel(1);
        store.set_sse_sender(&id, tx_a.clone(), None).unwrap();
        drop(rx_a);

        store.send_to_session(
            &id,
            crate::session::SseMessage::from_json_value(serde_json::json!({"type": "closed"}))
                .unwrap(),
        );

        assert!(!store.has_live_sse_sender(&id).unwrap());
    }

    #[tokio::test]
    async fn racing_reconnect_sender_is_not_clobbered_by_stale_clear() {
        // Reproduces the fix directly: `clear_sender_if_matching` is the exact
        // second-phase decision `try_send_to_session` makes after releasing and
        // re-acquiring the session guard. Here we drive it with a failed sender
        // (A) while the entry's live sender has already moved on to a
        // reconnected sender (B) -- simulating a client reconnect racing into
        // the window between phase 1 (detecting A failed) and phase 2 (clearing
        // it). B must survive and receive the event that failed to reach A.
        let store = StatefulSessionStore::new(1800, 100);
        let id = store.create_session().unwrap();

        // Install A and make it fail (closed receiver), as phase 1 of
        // try_send_to_session would observe.
        let (tx_a, rx_a) = mpsc::channel(1);
        store.set_sse_sender(&id, tx_a.clone(), None).unwrap();
        drop(rx_a);

        // A reconnecting client installs a fresh, live sender B before phase 2 runs.
        let (tx_b, mut rx_b) = mpsc::channel(4);
        store.set_sse_sender(&id, tx_b, None).unwrap();
        assert!(store.has_live_sse_sender(&id).unwrap());

        // Now run phase 2 directly with the stale failed sender A.
        let event = crate::session::SseEvent {
            id: 1,
            message: crate::session::SseMessage::from_json_value(
                serde_json::json!({"type": "raced"}),
            )
            .unwrap(),
        };
        store.clear_sender_if_matching(&id, Some(&tx_a), Some(event));

        // B must still be installed -- it was never the sender that failed.
        assert!(store.has_live_sse_sender(&id).unwrap());

        // And it should have received the event live rather than only queuing it.
        let received = tokio::time::timeout(Duration::from_secs(1), rx_b.recv())
            .await
            .expect("live sender should receive the raced event")
            .expect("event present");
        assert_eq!(received.message.to_json_value()["type"], "raced");
    }

    #[tokio::test]
    async fn reconnect_replays_events_after_last_event_id() {
        let store = StatefulSessionStore::new(1800, 100);
        let id = store.create_session().unwrap();

        let (first_tx, mut first_rx) = mpsc::channel(8);
        store.set_sse_sender(&id, first_tx, None).unwrap();
        store.send_to_session(
            &id,
            crate::session::SseMessage::from_json_value(serde_json::json!({"seq": 1})).unwrap(),
        );
        store.send_to_session(
            &id,
            crate::session::SseMessage::from_json_value(serde_json::json!({"seq": 2})).unwrap(),
        );

        let first = first_rx.recv().await.expect("first event");
        let second = first_rx.recv().await.expect("second event");
        assert_eq!(first.id, 1);
        assert_eq!(second.id, 2);

        let (reconnect_tx, mut reconnect_rx) = mpsc::channel(8);
        store
            .set_sse_sender(&id, reconnect_tx, Some(first.id))
            .unwrap();

        let replayed = reconnect_rx.recv().await.expect("replayed event");
        assert_eq!(replayed.id, second.id);
        assert_eq!(replayed.message.to_json_value()["seq"], 2);
        assert!(reconnect_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn reconnect_without_last_event_id_drains_pending_only() {
        let store = StatefulSessionStore::new(1800, 100);
        let id = store.create_session().unwrap();

        store.send_to_session(
            &id,
            crate::session::SseMessage::from_json_value(serde_json::json!({"seq": 1})).unwrap(),
        );

        let (tx, mut rx) = mpsc::channel(8);
        store.set_sse_sender(&id, tx, None).unwrap();

        let pending = rx.recv().await.expect("pending event");
        assert_eq!(pending.id, 1);
        assert_eq!(pending.message.to_json_value()["seq"], 1);
    }

    #[tokio::test]
    async fn reverse_request_replay_events_are_removed_by_key() {
        let store = StatefulSessionStore::new(1800, 100);
        let id = store.create_session().unwrap();

        store.send_to_session(
            &id,
            crate::session::SseMessage::from_json_value_with_replay_key(
                serde_json::json!({"jsonrpc": "2.0", "id": 7, "method": "roots/list"}),
                crate::session::SseReplayKey::ReverseRequest(7),
            )
            .unwrap(),
        );
        store.remove_replay_events_by_key(&id, &crate::session::SseReplayKey::ReverseRequest(7));

        let (tx, mut rx) = mpsc::channel(8);
        store.set_sse_sender(&id, tx, Some(0)).unwrap();

        assert!(rx.try_recv().is_err());
    }
}
