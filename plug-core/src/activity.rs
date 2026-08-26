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
    /// Stable per-connection client identity. For `plug connect` this is a
    /// per-process UUID, which is what distinguishes one editor window from
    /// another; it is not meant to be shown to a person on its own.
    pub client: Option<String>,
    pub server: Option<String>,
    pub method: String,
    /// Merged tool name for `tools/call`, absent for every other method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Detected client family (`claude-code`, `cursor`, ...) recorded at call
    /// time so an event stays attributable after its session disconnects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_type: Option<String>,
    /// Client-declared name and version from MCP initialize, when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_label: Option<String>,
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
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .filter(|event| event.sequence > filter.after_sequence)
            .filter(|event| !filter.failures_only || event.outcome != ActivityOutcome::Success)
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        events.reverse();
        events
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
            method: "tools/call".to_string(),
            tool: Some(format!("fixture__tool_{index}")),
            client_type: Some("codex-cli".into()),
            client_label: Some("codex 1.0".into()),
            latency_ms: 1,
            outcome: ActivityOutcome::Success,
        }
    }

    #[test]
    fn tool_calls_carry_attribution_without_payload_metadata() {
        let store = ActivityStore::default();
        store.record(event(0));
        let recorded = store.snapshot(&ActivityFilter::default()).remove(0);
        assert_eq!(recorded.tool.as_deref(), Some("fixture__tool_0"));
        assert_eq!(recorded.client_type.as_deref(), Some("codex-cli"));
        assert_eq!(recorded.client_label.as_deref(), Some("codex 1.0"));
        assert_eq!(recorded.server.as_deref(), Some("fixture"));
    }

    #[test]
    fn non_tool_events_omit_tool_attribution_from_the_wire() {
        let store = ActivityStore::default();
        store.record(ActivityEvent {
            method: "resources/list".to_string(),
            tool: None,
            client_type: None,
            client_label: None,
            ..event(0)
        });
        let json = serde_json::to_string(&store.snapshot(&ActivityFilter::default())).unwrap();
        assert!(!json.contains("\"tool\""), "{json}");
        assert!(!json.contains("client_type"), "{json}");
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

    #[test]
    fn a_bounded_snapshot_returns_the_newest_matching_events_in_order() {
        let store = ActivityStore::default();
        for index in 0..10 {
            store.record(event(index));
        }
        let events = store.snapshot(&ActivityFilter {
            after_sequence: 0,
            failures_only: false,
            limit: 3,
        });
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![8, 9, 10]
        );
    }
}
