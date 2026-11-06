//! Events Plug makes by watching a tool: call it on a schedule, compare the
//! result with the last one, and tell subscribed clients when it differs.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::hub::{Access, Hub, Pending, Source, SourceState};
use crate::config::{MIN_WATCH_SECS, WatchConfig};

use super::{
    EventDelivery, EventError, EventSource, MAX_BODY, invalid, iso_time, now, unavailable,
};

/// How many watched tools are called at once. The rest wait their turn.
pub(super) const CHECKS_AT_ONCE: usize = 8;
/// How often the watches are looked over for ones that are due.
const CHECK_TICK: Duration = Duration::from_secs(5);

/// How a watch is doing, for `plug events` and the app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchHealth {
    /// Not checked yet, or events are not possible with this config.
    #[default]
    Waiting,
    /// The last check worked.
    Watching,
    /// The server has no tool by that name, or the server is down.
    ToolMissing,
    /// The tool is not marked read-only and the watch does not allow writes.
    NotReadOnly,
    /// The last call failed or returned an error.
    CallFailed,
    /// The last result was too large to send.
    TooLarge,
}

/// One event a client can subscribe to, and how it is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventStatus {
    /// `<server>.<name>`.
    pub name: String,
    pub server: String,
    /// The watched tool. `None` for an event Plug does not make by watching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_secs: Option<u64>,
    #[serde(default)]
    pub state: WatchHealth,
    /// Unix seconds of the last check that reached the tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<u64>,
    /// Unix seconds of the last change that became an event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_changed: Option<u64>,
    #[serde(default)]
    pub subscribers: usize,
}

impl EventStatus {
    /// A configured watch nothing is known about yet.
    pub fn waiting(watch: &WatchConfig) -> Self {
        Self {
            name: watch.event_name(),
            server: watch.server.clone(),
            tool: Some(watch.tool.clone()),
            every_secs: Some(watch.every_secs),
            state: WatchHealth::Waiting,
            last_checked: None,
            last_changed: None,
            subscribers: 0,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct Checked {
    state: WatchHealth,
    pub(super) last_checked: Option<u64>,
    last_changed: Option<u64>,
}

/// What a watch needs from the running daemon. Kept behind a trait so the
/// engine is tested without servers, grants, or a network.
#[allow(
    clippy::double_must_use,
    reason = "async_trait generates redundant must_use on boxed futures"
)]
#[async_trait::async_trait]
pub trait WatchAccess: Send + Sync {
    /// The watches in the current config. Empty when events are not possible.
    fn watches(&self) -> Vec<WatchConfig>;
    /// Whether `client_id` is kept from neither the watch's server nor the
    /// tool it watches. An event carries that tool's result.
    fn may_use(&self, client_id: &str, watch: &WatchConfig) -> bool;
    /// Whether `client_id` holds the event scope and may use the watch.
    async fn permits(&self, client_id: &str, watch: &WatchConfig) -> bool;
    /// The name the tool is called by, and whether its server marks it read-only.
    fn tool(&self, watch: &WatchConfig) -> Option<(String, bool)>;
    /// The tool's result, or why there is none. The reason goes to the log,
    /// so it never carries the tool's output.
    async fn call(&self, tool_name: &str, arguments: Map<String, Value>) -> Result<Value, String>;
}

pub struct WatchSource {
    pub(super) access: Arc<dyn WatchAccess>,
    /// How each watch's last check went. Not persisted: it describes this run.
    pub(super) checked: std::sync::Mutex<HashMap<String, Checked>>,
    /// Watches with a check under way, so a slow one is not started twice.
    pub(super) checking: std::sync::Mutex<HashSet<String>>,
    /// How many watched tools may be called at once.
    pub(super) check_slots: Arc<tokio::sync::Semaphore>,
}

pub type WatchEvents = Hub<WatchSource>;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct WatchState {
    /// Event name to the hash of the last result seen.
    pub(super) baselines: BTreeMap<String, String>,
}

impl SourceState for WatchState {}

impl WatchSource {
    fn watch(&self, event: &str) -> Option<WatchConfig> {
        self.access
            .watches()
            .into_iter()
            .find(|watch| watch.event_name() == event)
    }

    fn note(&self, event: &str, state: WatchHealth, changed: bool) {
        let mut checked = self.checked.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = checked.entry(event.to_owned()).or_default();
        entry.state = state;
        if !matches!(state, WatchHealth::ToolMissing | WatchHealth::NotReadOnly) {
            entry.last_checked = Some(now());
        }
        if changed {
            entry.last_changed = Some(now());
        }
    }
}

#[async_trait::async_trait]
impl Source for WatchSource {
    type State = WatchState;
    const STATE_VERSION: u8 = 1;
    const MAX_SUBSCRIPTIONS: usize = 32;
    const LABEL: &'static str = "watch";

    fn discoverable_to(&self, client_id: &str) -> bool {
        !self.catalog(client_id).is_empty()
    }

    fn catalog(&self, client_id: &str) -> Vec<Value> {
        self.access
            .watches()
            .iter()
            .filter(|watch| self.access.may_use(client_id, watch))
            .map(|watch| {
                json!({
                    "name": watch.event_name(),
                    "description": format!(
                        "The result of {} on {} changed. Checked every {} seconds.",
                        watch.tool, watch.server, watch.every_secs
                    ),
                    "delivery": ["webhook"],
                    "inputSchema": {"type": "object", "additionalProperties": false},
                    "payloadSchema": {
                        "type": "object",
                        "properties": {"result": {}},
                        "required": ["result"],
                        "additionalProperties": false,
                    },
                })
            })
            .collect()
    }

    fn owns(&self, event: &str) -> bool {
        self.watch(event).is_some()
    }

    fn resolve(&self, name: &Value, arguments: &Value) -> Result<String, EventError> {
        let watch = name
            .as_str()
            .and_then(|name| self.watch(name))
            .ok_or_else(|| invalid("unsupported event"))?;
        if !arguments.is_null() && arguments.as_object().is_none_or(|a| !a.is_empty()) {
            return Err(invalid("this event takes no arguments"));
        }
        Ok(watch.event_name())
    }

    fn subscription_id(
        &self,
        client_id: &str,
        event: &str,
        url: &str,
    ) -> Result<String, EventError> {
        let key = serde_json::to_vec(&json!([client_id, event, url])).map_err(|_| unavailable())?;
        Ok(format!("sub_{}", hex::encode(Sha256::digest(key))))
    }

    async fn permits(&self, client_id: &str, event: &str) -> Access {
        match self.watch(event) {
            Some(watch) if self.access.permits(client_id, &watch).await => Access::Allowed,
            // Removed, or the client is kept from the server or the tool.
            _ => Access::Denied,
        }
    }

    fn tidy(&self, state: &mut WatchState) -> bool {
        let events: HashSet<String> = self
            .access
            .watches()
            .iter()
            .map(WatchConfig::event_name)
            .collect();
        let before = state.baselines.len();
        state.baselines.retain(|event, _| events.contains(event));
        state.baselines.len() != before
    }

    fn start(hub: &Arc<Hub<Self>>, cancel: &CancellationToken) {
        let hub = Arc::clone(hub);
        let cancel = cancel.clone();
        tokio::spawn(async move {
            // When each watch is next due. A watch added later is due at once.
            let mut due: HashMap<String, u64> = HashMap::new();
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = async {
                        let watches = hub.source().access.watches();
                        due.retain(|event, _| watches.iter().any(|w| &w.event_name() == event));
                        if let Err(error) = hub.prune().await {
                            tracing::warn!(code = error.code, "event state unavailable");
                        }
                        hub.check_due(&watches, &mut due, &cancel);
                        tokio::time::sleep(CHECK_TICK).await;
                    } => {}
                }
            }
        });
    }
}

/// Marks a watch as no longer being checked, however its check ended.
struct Checking {
    events: Arc<WatchEvents>,
    event: String,
}

impl Drop for Checking {
    fn drop(&mut self) {
        self.events
            .source()
            .checking
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.event);
    }
}

impl Hub<WatchSource> {
    pub fn open(
        dir: &Path,
        access: Arc<dyn WatchAccess>,
        sender: Arc<dyn EventDelivery>,
    ) -> anyhow::Result<Arc<Self>> {
        let source = WatchSource {
            access,
            checked: std::sync::Mutex::default(),
            checking: std::sync::Mutex::default(),
            check_slots: Arc::new(tokio::sync::Semaphore::new(CHECKS_AT_ONCE)),
        };
        Hub::load(dir, source, sender)
    }

    /// Fill in what this run knows about `status`'s watch.
    pub async fn describe(&self, status: &mut EventStatus) {
        let checked = self
            .source()
            .checked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&status.name)
            .copied();
        // A watch the engine is not running has no state worth showing.
        if let Some(checked) = checked.filter(|_| self.owns(&status.name)) {
            status.state = checked.state;
            status.last_checked = checked.last_checked;
            status.last_changed = checked.last_changed;
        }
        status.subscribers = self
            .state()
            .await
            .live(now())
            .filter(|s| s.event == status.name)
            .count();
    }

    /// Start a check of each of `watches` whose time has come and note in
    /// `due` when it is next due. Does not wait for the checks: a tool that
    /// is slow to answer delays neither the watches due with it nor anyone's
    /// next round. A watch still being checked from last time is left alone.
    /// A check that waited its turn asks again whether the watch is still
    /// configured, and `cancel` stops every check, waiting or under way.
    pub(super) fn check_due(
        self: &Arc<Self>,
        watches: &[WatchConfig],
        due: &mut HashMap<String, u64>,
        cancel: &CancellationToken,
    ) {
        for watch in watches {
            let event = watch.event_name();
            if due.get(&event).is_some_and(|at| *at > now()) {
                continue;
            }
            if !self
                .source()
                .checking
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(event.clone())
            {
                continue;
            }
            due.insert(event.clone(), now() + watch.every_secs.max(MIN_WATCH_SECS));
            let checking = Checking {
                events: Arc::clone(self),
                event,
            };
            let watch = watch.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let events = &checking.events;
                let check = async {
                    let Ok(_slot) = events.source().check_slots.acquire().await else {
                        return;
                    };
                    // Removed, changed, or its server switched off while
                    // this check waited for a slot.
                    if !events.source().access.watches().contains(&watch) {
                        return;
                    }
                    if let Err(error) = events.check(&watch).await {
                        tracing::warn!(code = error.code, "watch paused");
                    }
                };
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {}
                    _ = check => {}
                }
            });
        }
    }

    /// Call one watched tool and queue an event for each subscriber when its
    /// result differs from the last one. The first result is the baseline.
    pub(super) async fn check(&self, watch: &WatchConfig) -> Result<(), EventError> {
        let source = self.source();
        let event = watch.event_name();
        let Some((tool_name, read_only)) = source.access.tool(watch) else {
            source.note(&event, WatchHealth::ToolMissing, false);
            return Ok(());
        };
        if !read_only && !watch.allow_writes {
            source.note(&event, WatchHealth::NotReadOnly, false);
            tracing::warn!(
                event = %event,
                "watch skipped: the tool is not marked read-only and allow_writes is off"
            );
            return Ok(());
        }
        let result = match source
            .access
            .call(&tool_name, watch.arguments.clone())
            .await
        {
            Ok(result) => result,
            Err(reason) => {
                tracing::info!(event = %event, %reason, "watch call failed");
                source.note(&event, WatchHealth::CallFailed, false);
                return Ok(());
            }
        };
        let timestamp = now();
        let event_id = format!("evt_{}", uuid::Uuid::new_v4());
        let body = serde_json::to_vec(&json!({
            "eventId": event_id,
            "name": event,
            "timestamp": iso_time(timestamp)?,
            "cursor": null,
            "data": {"result": result},
        }))
        .map_err(|_| unavailable())?;
        if body.len() > MAX_BODY {
            tracing::warn!(event = %event, "watch skipped: the result exceeds the size limit");
            source.note(&event, WatchHealth::TooLarge, false);
            return Ok(());
        }
        let hash = hex::encode(Sha256::digest(
            serde_json::to_vec(&result).map_err(|_| unavailable())?,
        ));
        let mut state = self.state().await;
        let previous = state.source.baselines.get(&event);
        if previous == Some(&hash) {
            source.note(&event, WatchHealth::Watching, false);
            return Ok(());
        }
        let changed = previous.is_some();
        let mut next = state.clone();
        next.source.baselines.insert(event.clone(), hash);
        if changed {
            let subscribers: Vec<String> = next
                .live(timestamp)
                .filter(|s| s.event == event)
                .map(|s| s.id.clone())
                .collect();
            for subscription_id in subscribers {
                if next.queue.len() >= super::MAX_QUEUE {
                    tracing::warn!(event = %event, "event dropped: the delivery queue is full");
                    break;
                }
                next.queue.push(Pending::new(
                    subscription_id,
                    event_id.clone(),
                    None,
                    body.clone(),
                    timestamp,
                ));
            }
        }
        self.commit(&mut state, next)?;
        source.note(&event, WatchHealth::Watching, changed);
        self.wake();
        Ok(())
    }
}
