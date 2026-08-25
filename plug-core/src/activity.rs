//! Bounded, metadata-only operator activity.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

pub const ACTIVITY_CAPACITY: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityOutcome {
    Success,
    Error,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEvent {
    pub sequence: u64,
    pub occurred_at_ms: u64,
    pub client: Option<String>,
    pub server: Option<String>,
    pub method: String,
    pub latency_ms: u64,
    pub outcome: ActivityOutcome,
}

#[derive(Debug, Clone, Default)]
pub struct ActivityFilter {
    pub after_sequence: u64,
    pub failures_only: bool,
    pub limit: usize,
}

pub struct ActivityStore {
    next_sequence: AtomicU64,
    events: Mutex<VecDeque<ActivityEvent>>,
    tx: broadcast::Sender<ActivityEvent>,
}

impl Default for ActivityStore {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(ACTIVITY_CAPACITY);
        Self {
            next_sequence: AtomicU64::new(1),
            events: Mutex::new(VecDeque::with_capacity(ACTIVITY_CAPACITY)),
            tx,
        }
    }
}

impl ActivityStore {
    pub fn record(&self, mut event: ActivityEvent) {
        event.sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed);
        event.occurred_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if events.len() == ACTIVITY_CAPACITY {
            events.pop_front();
        }
        events.push_back(event.clone());
        drop(events);
        let _ = self.tx.send(event);
    }

    pub fn snapshot(&self, filter: &ActivityFilter) -> Vec<ActivityEvent> {
        let limit = if filter.limit == 0 {
            ACTIVITY_CAPACITY
        } else {
            filter.limit.min(ACTIVITY_CAPACITY)
        };
        self.events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .filter(|event| event.sequence > filter.after_sequence)
            .filter(|event| !filter.failures_only || event.outcome != ActivityOutcome::Success)
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ActivityEvent> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(index: usize) -> ActivityEvent {
        ActivityEvent {
            sequence: 0,
            occurred_at_ms: 0,
            client: Some("codex".into()),
            server: Some("fixture".into()),
            method: format!("tools/call/{index}"),
            latency_ms: 1,
            outcome: ActivityOutcome::Success,
        }
    }

    #[test]
    fn keeps_only_newest_500_metadata_events() {
        let store = ActivityStore::default();
        for index in 0..501 {
            store.record(event(index));
        }
        let events = store.snapshot(&ActivityFilter::default());
        assert_eq!(events.len(), 500);
        assert_eq!(events.first().unwrap().sequence, 2);
        let json = serde_json::to_string(&events).unwrap();
        for forbidden in ["params", "result", "token", "secret"] {
            assert!(!json.contains(forbidden));
        }
    }
}
