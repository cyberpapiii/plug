//! Events Plug makes by watching a tool: call it on a schedule, compare the
//! result with the last one, and tell subscribed clients when it differs.
//!
//! Subscription and webhook delivery follow the same MCP Events contract the
//! Slack source does (`crate::slack_events`), and reuse its signing, callback
//! validation, and state-file rules.
mod access;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use fs4::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, Notify, RwLock};
use tokio_util::sync::CancellationToken;

pub use access::RuntimeWatchAccess;

use crate::slack_events::{
    DAY, EventDelivery, EventError, MAX_ATTEMPTS, MAX_BODY, MAX_QUEUE, callback_error,
    check_private, denied, invalid, iso_time, now, private_open, signed_headers, signing_key,
    unavailable, validate_url,
};

/// The shortest interval a watch may ask for.
pub const MIN_WATCH_SECS: u64 = 30;
const MAX_SUBSCRIPTIONS: usize = 32;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsConfig {
    /// Tools watched for change. Each one is an event clients can subscribe to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch: Vec<WatchConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchConfig {
    /// The event is named `<server>.<name>`.
    pub name: String,
    pub server: String,
    /// The tool's own name on that server, without Plug's prefix.
    pub tool: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub arguments: Map<String, Value>,
    #[serde(default = "default_every_secs")]
    pub every_secs: u64,
    /// Watch a tool the server does not mark read-only. Off by default,
    /// because watching must not change anything.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_writes: bool,
}

fn default_every_secs() -> u64 {
    300
}

impl WatchConfig {
    pub fn event_name(&self) -> String {
        format!("{}.{}", self.server, self.name)
    }
}

impl EventsConfig {
    pub fn is_empty(&self) -> bool {
        self.watch.is_empty()
    }

    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let mut names = std::collections::HashSet::new();
        for watch in &self.watch {
            let event = watch.event_name();
            if watch.name.is_empty()
                || watch.name.len() > 64
                || !watch
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                errors.push(format!(
                    "events.watch name '{}' must be lowercase letters, digits, and underscores",
                    watch.name
                ));
            }
            if watch.tool.is_empty() {
                errors.push(format!("events.watch '{event}' names no tool"));
            }
            if watch.every_secs < MIN_WATCH_SECS {
                errors.push(format!(
                    "events.watch '{event}' every_secs must be at least {MIN_WATCH_SECS}"
                ));
            }
            if event == crate::slack_events::EVENT_NAME || !names.insert(event.clone()) {
                errors.push(format!("events.watch '{event}' is already an event"));
            }
        }
        errors
    }
}

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
struct Checked {
    state: WatchHealth,
    last_checked: Option<u64>,
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
    /// The tool's result, or `None` when the call failed or returned an error.
    async fn call(&self, tool_name: &str, arguments: Map<String, Value>) -> Option<Value>;
}

#[derive(Clone, Serialize, Deserialize)]
struct Subscription {
    id: String,
    client_id: String,
    event: String,
    url: String,
    secret: crate::types::SecretString,
    old_secret: Option<crate::types::SecretString>,
    old_secret_until: u64,
    expires: u64,
    verified_until: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    subscription_id: String,
    event_id: String,
    body: Vec<u8>,
    attempts: u32,
    next_at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct WatchState {
    version: u8,
    subscriptions: Vec<Subscription>,
    /// Event name to the hash of the last result seen.
    baselines: BTreeMap<String, String>,
    queue: Vec<Pending>,
}

impl Default for WatchState {
    fn default() -> Self {
        Self {
            version: 1,
            subscriptions: Vec::new(),
            baselines: BTreeMap::new(),
            queue: Vec::new(),
        }
    }
}

/// Secrets and tool results never implement Debug or enter diagnostics.
pub struct WatchEvents {
    path: PathBuf,
    _lock: File,
    state: Mutex<WatchState>,
    /// Serialize unsubscribe and refresh against an in-flight send.
    delivery_gate: RwLock<()>,
    access: Arc<dyn WatchAccess>,
    sender: Arc<dyn EventDelivery>,
    degraded: AtomicBool,
    wake: Notify,
    /// How each watch's last check went. Not persisted: it describes this run.
    checked: std::sync::Mutex<HashMap<String, Checked>>,
    /// Watches with a check under way, so a slow one is not started twice.
    checking: std::sync::Mutex<std::collections::HashSet<String>>,
    /// How many watched tools may be called at once.
    check_slots: Arc<tokio::sync::Semaphore>,
}

/// How many watched tools are called at once. The rest wait their turn.
const CHECKS_AT_ONCE: usize = 8;

/// Marks a watch as no longer being checked, however its check ended.
struct Checking {
    events: Arc<WatchEvents>,
    event: String,
}

impl Drop for Checking {
    fn drop(&mut self) {
        if let Ok(mut checking) = self.events.checking.lock() {
            checking.remove(&self.event);
        }
    }
}

impl WatchEvents {
    pub fn open(
        dir: &Path,
        access: Arc<dyn WatchAccess>,
        sender: Arc<dyn EventDelivery>,
    ) -> anyhow::Result<Arc<Self>> {
        crate::fs_perm::ensure_dir_0700(dir)?;
        check_private(dir, true)?;
        let path = dir.join("state.json");
        let lock_path = dir.join("state.lock");
        let lock = private_open(&lock_path, false)?;
        check_private(&lock_path, false)?;
        FileExt::try_lock(&lock).map_err(|_| anyhow::anyhow!("event state already in use"))?;
        let state = if path.exists() {
            check_private(&path, false)?;
            let bytes = std::fs::read(&path)?;
            anyhow::ensure!(bytes.len() <= 8 * 1024 * 1024, "event state exceeds limit");
            let value: WatchState = serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("invalid event state"))?;
            anyhow::ensure!(
                value.version == 1
                    && value.queue.len() <= MAX_QUEUE
                    && value.subscriptions.len() <= MAX_SUBSCRIPTIONS,
                "unsupported event state"
            );
            value
        } else {
            WatchState::default()
        };
        Ok(Arc::new(Self {
            path,
            _lock: lock,
            state: Mutex::new(state),
            delivery_gate: RwLock::new(()),
            access,
            sender,
            degraded: AtomicBool::new(false),
            wake: Notify::new(),
            checked: std::sync::Mutex::new(HashMap::new()),
            checking: std::sync::Mutex::default(),
            check_slots: Arc::new(tokio::sync::Semaphore::new(CHECKS_AT_ONCE)),
        }))
    }

    fn watch(&self, event: &str) -> Option<WatchConfig> {
        self.access
            .watches()
            .into_iter()
            .find(|watch| watch.event_name() == event)
    }

    /// Whether `event` is one of the watches in the current config.
    pub fn owns(&self, event: &str) -> bool {
        !self.degraded.load(Ordering::Acquire) && self.watch(event).is_some()
    }

    /// Whether `client_id` can see any watch, so the capability is worth
    /// advertising to it.
    pub fn discoverable_to(&self, client_id: &str) -> bool {
        !self.catalog(client_id).is_empty()
    }

    /// The event definitions `events/list` returns to `client_id`.
    pub fn catalog(&self, client_id: &str) -> Vec<Value> {
        if self.degraded.load(Ordering::Acquire) {
            return Vec::new();
        }
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

    fn note(&self, event: &str, state: WatchHealth, changed: bool) {
        let mut checked = self.checked.lock().unwrap_or_else(|e| e.into_inner());
        let entry = checked.entry(event.to_owned()).or_default();
        entry.state = state;
        if !matches!(state, WatchHealth::ToolMissing | WatchHealth::NotReadOnly) {
            entry.last_checked = Some(now());
        }
        if changed {
            entry.last_changed = Some(now());
        }
    }

    /// Fill in what this run knows about `status`'s watch.
    pub async fn describe(&self, status: &mut EventStatus) {
        let checked = self
            .checked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&status.name)
            .copied();
        // A watch the engine is not running has no state worth showing.
        if let Some(checked) = checked.filter(|_| self.owns(&status.name)) {
            status.state = checked.state;
            status.last_checked = checked.last_checked;
            status.last_changed = checked.last_changed;
        }
        let timestamp = now();
        status.subscribers = self
            .state
            .lock()
            .await
            .subscriptions
            .iter()
            .filter(|s| s.event == status.name && s.expires > timestamp)
            .count();
    }

    fn identity(&self, client_id: &str, params: &Value) -> Result<Subscribing, EventError> {
        let object = params
            .as_object()
            .ok_or_else(|| invalid("event params must be an object"))?;
        if object.keys().any(|k| {
            !["name", "arguments", "delivery", "ttlMs", "cursor", "_meta"].contains(&k.as_str())
        }) {
            return Err(invalid("unknown event parameter"));
        }
        let watch = params["name"]
            .as_str()
            .and_then(|name| self.watch(name))
            .ok_or_else(|| invalid("unsupported event"))?;
        if !matches!(&params["arguments"], Value::Null)
            && params["arguments"]
                .as_object()
                .is_none_or(|a| !a.is_empty())
        {
            return Err(invalid("this event takes no arguments"));
        }
        let delivery = params["delivery"]
            .as_object()
            .ok_or_else(|| invalid("webhook delivery is required"))?;
        if delivery
            .keys()
            .any(|k| !["mode", "url", "secret"].contains(&k.as_str()))
            || params["delivery"]["mode"] != "webhook"
        {
            return Err(invalid("only webhook delivery is supported"));
        }
        let url = params["delivery"]["url"]
            .as_str()
            .ok_or_else(|| invalid("callback URL is required"))?;
        validate_url(url).map_err(callback_error)?;
        let event = watch.event_name();
        let key = serde_json::to_vec(&json!([client_id, event, url])).map_err(|_| unavailable())?;
        Ok(Subscribing {
            id: format!("sub_{}", hex::encode(Sha256::digest(key))),
            event,
            watch,
            url: url.to_owned(),
        })
    }

    pub async fn subscribe(&self, client_id: &str, params: &Value) -> Result<Value, EventError> {
        if self.degraded.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        let target = self.identity(client_id, params)?;
        if params.get("cursor").is_some_and(|v| !v.is_null()) {
            return Err(invalid("this event does not support replay"));
        }
        let secret = params["delivery"]["secret"]
            .as_str()
            .ok_or_else(|| invalid("signing secret is required"))?;
        signing_key(secret)?;
        let ttl = match params.get("ttlMs") {
            None | Some(Value::Null) => DAY * 1000,
            Some(v) => v
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or_else(|| invalid("ttlMs must be positive or null"))?
                .min(DAY * 1000),
        };
        if ttl < 1000 {
            return Err(invalid("minimum supported ttlMs is 1000"));
        }
        let _gate = self.delivery_gate.write().await;
        if !self.access.permits(client_id, &target.watch).await {
            return Err(denied());
        }
        let current = {
            let state = self.state.lock().await;
            let live = state
                .subscriptions
                .iter()
                .filter(|s| s.id != target.id && s.expires > now())
                .count();
            if live >= MAX_SUBSCRIPTIONS {
                return Err(invalid("too many event subscriptions"));
            }
            state
                .subscriptions
                .iter()
                .find(|s| s.id == target.id)
                .cloned()
        };
        let same_secret =
            |s: &Subscription| bool::from(s.secret.as_str().as_bytes().ct_eq(secret.as_bytes()));
        let verified = current
            .as_ref()
            .is_some_and(|s| same_secret(s) && s.verified_until > now());
        if !verified {
            let challenge = uuid::Uuid::new_v4().to_string();
            let body = serde_json::to_vec(&json!({"type":"verification","challenge":challenge}))
                .map_err(|_| unavailable())?;
            let verification_id = format!("msg_verification_{}", uuid::Uuid::new_v4());
            let headers = signed_headers(&target.id, &verification_id, secret, None, &body, now())?;
            let response = self
                .sender
                .post(&target.url, headers, body)
                .await
                .map_err(callback_error)?;
            let echoed: Value = serde_json::from_slice(&response.body)
                .map_err(|_| callback_error("challenge_failed"))?;
            if !(200..300).contains(&response.status)
                || !echoed["challenge"]
                    .as_str()
                    .is_some_and(|v| bool::from(v.as_bytes().ct_eq(challenge.as_bytes())))
            {
                return Err(callback_error("challenge_failed"));
            }
        }
        let timestamp = now();
        let live = current.as_ref().filter(|s| s.expires > timestamp);
        // A new secret on refresh rotates it; both sign for five minutes.
        let old_secret = live
            .filter(|s| !same_secret(s))
            .map(|s| s.secret.clone())
            .or_else(|| {
                live.filter(|s| s.old_secret_until > timestamp)
                    .and_then(|s| s.old_secret.clone())
            });
        let old_secret_until = live
            .filter(|s| same_secret(s))
            .map_or(timestamp + 300, |s| s.old_secret_until);
        let mut state = self.state.lock().await;
        let mut next = state.clone();
        next.subscriptions.retain(|s| s.id != target.id);
        if live.is_none() {
            next.queue.retain(|p| p.subscription_id != target.id);
        }
        next.subscriptions.push(Subscription {
            id: target.id.clone(),
            client_id: client_id.to_owned(),
            event: target.event,
            url: target.url,
            secret: secret.to_owned().into(),
            old_secret,
            old_secret_until,
            expires: timestamp + ttl / 1000,
            verified_until: timestamp + 300,
        });
        self.commit(&mut state, next)?;
        Ok(json!({
            "id": target.id,
            "refreshBefore": iso_time(timestamp + ttl / 1000)?,
            "cursor": null,
            "truncated": false,
        }))
    }

    pub async fn unsubscribe(&self, client_id: &str, params: &Value) -> Result<Value, EventError> {
        let target = self.identity(client_id, params)?;
        let _gate = self.delivery_gate.write().await;
        let mut state = self.state.lock().await;
        if state.subscriptions.iter().any(|s| s.id == target.id) {
            let mut next = state.clone();
            next.subscriptions.retain(|s| s.id != target.id);
            next.queue.retain(|p| p.subscription_id != target.id);
            self.commit(&mut state, next)?;
        }
        Ok(json!({}))
    }

    fn commit(&self, state: &mut WatchState, next: WatchState) -> Result<(), EventError> {
        if self.degraded.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        let bytes = serde_json::to_vec(&next).map_err(|_| unavailable())?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(unavailable());
        }
        let result = (|| -> std::io::Result<()> {
            let temp = self
                .path
                .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
            let mut file = private_open(&temp, true)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temp, &self.path)?;
            File::open(
                self.path
                    .parent()
                    .ok_or_else(|| std::io::Error::other("state directory missing"))?,
            )?
            .sync_all()
        })();
        if result.is_err() {
            self.degraded.store(true, Ordering::Release);
            return Err(unavailable());
        }
        *state = next;
        Ok(())
    }

    /// Start a check of each of `watches` whose time has come and note in
    /// `due` when it is next due. Does not wait for the checks: a tool that
    /// is slow to answer delays neither the watches due with it nor anyone's
    /// next round. A watch still being checked from last time is left alone.
    fn check_due(self: &Arc<Self>, watches: &[WatchConfig], due: &mut HashMap<String, u64>) {
        for watch in watches {
            let event = watch.event_name();
            if due.get(&event).is_some_and(|at| *at > now()) {
                continue;
            }
            if !self
                .checking
                .lock()
                .is_ok_and(|mut checking| checking.insert(event.clone()))
            {
                continue;
            }
            due.insert(event.clone(), now() + watch.every_secs.max(MIN_WATCH_SECS));
            let checking = Checking {
                events: Arc::clone(self),
                event,
            };
            let watch = watch.clone();
            tokio::spawn(async move {
                let events = &checking.events;
                let Ok(_slot) = events.check_slots.acquire().await else {
                    return;
                };
                if let Err(error) = events.check(&watch).await {
                    tracing::warn!(code = error.code, "watch paused");
                }
            });
        }
    }

    /// Call one watched tool and queue an event for each subscriber when its
    /// result differs from the last one. The first result is the baseline.
    async fn check(&self, watch: &WatchConfig) -> Result<(), EventError> {
        let event = watch.event_name();
        let Some((tool_name, read_only)) = self.access.tool(watch) else {
            self.note(&event, WatchHealth::ToolMissing, false);
            return Ok(());
        };
        if !read_only && !watch.allow_writes {
            self.note(&event, WatchHealth::NotReadOnly, false);
            tracing::warn!(
                event = %event,
                "watch skipped: the tool is not marked read-only and allow_writes is off"
            );
            return Ok(());
        }
        let Some(result) = self.access.call(&tool_name, watch.arguments.clone()).await else {
            self.note(&event, WatchHealth::CallFailed, false);
            return Ok(());
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
            self.note(&event, WatchHealth::TooLarge, false);
            return Ok(());
        }
        let hash = hex::encode(Sha256::digest(
            serde_json::to_vec(&result).map_err(|_| unavailable())?,
        ));
        let mut state = self.state.lock().await;
        let previous = state.baselines.get(&event);
        if previous == Some(&hash) {
            self.note(&event, WatchHealth::Watching, false);
            return Ok(());
        }
        let changed = previous.is_some();
        let mut next = state.clone();
        next.baselines.insert(event.clone(), hash);
        if changed {
            let subscribers: Vec<String> = next
                .subscriptions
                .iter()
                .filter(|s| s.event == event && s.expires > timestamp)
                .map(|s| s.id.clone())
                .collect();
            for subscription_id in subscribers {
                if next.queue.len() >= MAX_QUEUE {
                    tracing::warn!(event = %event, "event dropped: the delivery queue is full");
                    break;
                }
                next.queue.push(Pending {
                    subscription_id,
                    event_id: event_id.clone(),
                    body: body.clone(),
                    attempts: 0,
                    next_at: timestamp,
                });
            }
        }
        self.commit(&mut state, next)?;
        self.note(&event, WatchHealth::Watching, changed);
        self.wake.notify_one();
        Ok(())
    }

    /// Forget what belongs to watches and subscriptions that no longer exist.
    async fn prune(&self) -> Result<(), EventError> {
        let events: std::collections::HashSet<String> = self
            .access
            .watches()
            .iter()
            .map(WatchConfig::event_name)
            .collect();
        let timestamp = now();
        let _gate = self.delivery_gate.read().await;
        let mut state = self.state.lock().await;
        let mut next = state.clone();
        next.baselines.retain(|event, _| events.contains(event));
        next.subscriptions
            .retain(|s| events.contains(&s.event) && s.expires > timestamp);
        let live: std::collections::HashSet<&str> =
            next.subscriptions.iter().map(|s| s.id.as_str()).collect();
        next.queue
            .retain(|p| live.contains(p.subscription_id.as_str()));
        if next.baselines.len() == state.baselines.len()
            && next.subscriptions.len() == state.subscriptions.len()
            && next.queue.len() == state.queue.len()
        {
            return Ok(());
        }
        self.commit(&mut state, next)
    }

    async fn deliver_one(&self) -> Result<(), EventError> {
        let _gate = self.delivery_gate.read().await;
        let timestamp = now();
        let (subscription, pending) = {
            let state = self.state.lock().await;
            let Some(pending) = state.queue.iter().find(|p| p.next_at <= timestamp).cloned() else {
                return Ok(());
            };
            let subscription = state
                .subscriptions
                .iter()
                .find(|s| s.id == pending.subscription_id && s.expires > timestamp)
                .cloned();
            (subscription, pending)
        };
        let watch = subscription.as_ref().and_then(|s| self.watch(&s.event));
        let permitted = match (&subscription, &watch) {
            (Some(subscription), Some(watch)) => {
                self.access.permits(&subscription.client_id, watch).await
            }
            _ => false,
        };
        let Some(subscription) = subscription.filter(|_| permitted) else {
            // Expired, removed, revoked, or kept from the server or the
            // tool since.
            let mut state = self.state.lock().await;
            let mut next = state.clone();
            next.subscriptions
                .retain(|s| s.id != pending.subscription_id);
            next.queue
                .retain(|p| p.subscription_id != pending.subscription_id);
            return self.commit(&mut state, next);
        };
        let old = subscription
            .old_secret
            .as_ref()
            .filter(|_| subscription.old_secret_until > timestamp)
            .map(|s| s.as_str());
        let headers = signed_headers(
            &subscription.id,
            &pending.event_id,
            subscription.secret.as_str(),
            old,
            &pending.body,
            timestamp,
        )?;
        let status = self
            .sender
            .post(&subscription.url, headers, pending.body.clone())
            .await
            .ok()
            .map(|r| r.status);
        let mut state = self.state.lock().await;
        let mut next = state.clone();
        let this = |p: &Pending| {
            p.event_id == pending.event_id && p.subscription_id == pending.subscription_id
        };
        let accepted = status.is_some_and(|s| (200..300).contains(&s));
        let permanent = status.is_some_and(|s| (400..500).contains(&s) && ![408, 429].contains(&s));
        if accepted || permanent || pending.attempts + 1 >= MAX_ATTEMPTS {
            next.queue.retain(|p| !this(p));
            if status == Some(410) {
                next.subscriptions.retain(|s| s.id != subscription.id);
                next.queue.retain(|p| p.subscription_id != subscription.id);
            }
            if !accepted {
                tracing::warn!(status, "event delivery exhausted or rejected");
            }
        } else if let Some(job) = next.queue.iter_mut().find(|p| this(p)) {
            job.attempts += 1;
            job.next_at = now() + (1_u64 << job.attempts).min(300);
        }
        self.commit(&mut state, next)
    }

    /// Run the watches and the delivery worker until `cancel` fires.
    pub fn spawn(self: &Arc<Self>, cancel: CancellationToken) {
        let events = Arc::clone(self);
        let delivery_cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = delivery_cancel.cancelled() => break,
                    _ = async {
                        if let Err(error) = events.deliver_one().await {
                            tracing::warn!(code = error.code, "event delivery paused");
                        }
                        tokio::select! {
                            _ = events.wake.notified() => {},
                            _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                        }
                    } => {}
                }
            }
        });
        let events = Arc::clone(self);
        tokio::spawn(async move {
            // When each watch is next due. A watch added later is due at once.
            let mut due: HashMap<String, u64> = HashMap::new();
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = async {
                        let watches = events.access.watches();
                        due.retain(|event, _| watches.iter().any(|w| &w.event_name() == event));
                        if let Err(error) = events.prune().await {
                            tracing::warn!(code = error.code, "event state unavailable");
                        }
                        events.check_due(&watches, &mut due);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    } => {}
                }
            }
        });
    }
}

struct Subscribing {
    id: String,
    event: String,
    watch: WatchConfig,
    url: String,
}
