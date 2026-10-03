//! Opt-in one-public-channel source for OpenAI's MCP Events webhook contract.
//! This does not forward resource subscriptions or replace the native dot app.
mod access;
pub mod credentials;
mod delivery;
pub use access::RuntimeAccess;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::http::HeaderMap;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use fs4::FileExt;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, Notify, RwLock};
use tokio_util::sync::CancellationToken;

pub use delivery::HttpsDelivery;
pub const EVENT_NAME: &str = "slack.ditto_message";
pub const EVENT_SCOPE: &str = "events:subscribe";
pub const SOURCE_PATH: &str = "/events/slack";
const OWNER_PROOF_MARKER: &str = "PLUG_EVENTS_PROOF_20261003";
const DAY: u64 = 86_400;
const MAX_BODY: usize = 256 * 1024;
const MAX_QUEUE: usize = 128;
const MAX_SEEN: usize = 10_000;
const MAX_ATTEMPTS: u32 = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlackEventsConfig {
    pub team_id: String,
    pub app_id: String,
    pub owner_user_id: String,
    pub dot_user_id: String,
    /// Existing downstream OAuth registration explicitly chosen by the owner.
    pub subscriber_client_id: String,
    /// Temporary exact-marker owner test; Unix expiry, disabled by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_proof_until: Option<u64>,
}

impl SlackEventsConfig {
    pub fn validate(&self) -> Vec<String> {
        let valid_id = |value: &str, prefix| {
            value.starts_with(prefix)
                && value.len() > 1
                && value
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        };
        let mut errors = Vec::new();
        for (name, value, prefix) in [
            ("team_id", &self.team_id, 'T'),
            ("app_id", &self.app_id, 'A'),
            ("owner_user_id", &self.owner_user_id, 'U'),
            ("dot_user_id", &self.dot_user_id, 'U'),
        ] {
            if !valid_id(value, prefix) {
                errors.push(format!("http.slack_events.{name} is invalid"));
            }
        }
        if self.owner_user_id == self.dot_user_id || self.subscriber_client_id.is_empty() {
            errors.push(
                "http.slack_events requires distinct owner/dot users and an explicit OAuth client"
                    .into(),
            );
        }
        if self
            .owner_proof_until
            .is_some_and(|until| until > now().saturating_add(3600))
        {
            errors
                .push("http.slack_events.owner_proof_until cannot exceed one hour from now".into());
        }
        errors
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Allowed,
    Denied,
    Unavailable,
}

#[allow(
    clippy::double_must_use,
    reason = "async_trait generates redundant must_use on boxed futures"
)]
#[async_trait::async_trait]
pub trait EventAccess: Send + Sync {
    /// Checks current config before ingress/discovery without any network request.
    fn enabled(&self) -> bool;
    /// Recheck current downstream grant and current Slack account/channel visibility.
    async fn check(&self, channel: Option<&str>) -> Access;
}

pub struct DeliveryResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[allow(
    clippy::double_must_use,
    reason = "async_trait generates redundant must_use on boxed futures"
)]
#[async_trait::async_trait]
pub trait EventDelivery: Send + Sync {
    /// Production implementation validates/pins DNS per connection, preserves TLS
    /// hostname verification, forbids proxies/redirects, and bounds response bytes.
    async fn post(
        &self,
        url: &str,
        headers: HeaderMap,
        body: Vec<u8>,
    ) -> Result<DeliveryResponse, &'static str>;
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct EventError {
    pub code: i32,
    pub message: &'static str,
    pub reason: Option<&'static str>,
}
fn invalid(message: &'static str) -> EventError {
    EventError {
        code: -32602,
        message,
        reason: None,
    }
}
fn denied() -> EventError {
    EventError {
        code: -32001,
        message: "event access denied",
        reason: None,
    }
}
fn unavailable() -> EventError {
    EventError {
        code: -32603,
        message: "event state unavailable",
        reason: None,
    }
}
fn callback_error(reason: &'static str) -> EventError {
    EventError {
        code: -32015,
        message: "callback verification failed",
        reason: Some(reason),
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Subscription {
    id: String,
    url: String,
    secret: crate::types::SecretString,
    old_secret: Option<crate::types::SecretString>,
    old_secret_until: u64,
    expires: u64,
    verified_until: u64,
}
#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    event_id: String,
    channel_id: String,
    body: Vec<u8>,
    attempts: u32,
    next_at: u64,
}
#[derive(Clone, Serialize, Deserialize)]
struct EventState {
    version: u8,
    subscription: Option<Subscription>,
    seen: BTreeMap<String, u64>,
    threads: BTreeMap<String, u64>,
    queue: Vec<Pending>,
    source_revoked: bool,
}
impl Default for EventState {
    fn default() -> Self {
        Self {
            version: 2,
            subscription: None,
            seen: BTreeMap::new(),
            threads: BTreeMap::new(),
            queue: Vec::new(),
            source_revoked: false,
        }
    }
}

/// Secrets and message bodies never implement Debug or enter diagnostics.
pub struct SlackEvents {
    pub config: SlackEventsConfig,
    source_secret: crate::types::SecretString,
    path: PathBuf,
    _lock: File,
    state: Mutex<EventState>,
    /// Serialize unsubscribe/refresh against in-flight sends, without making Slack
    /// acknowledgements wait for outbound network calls.
    delivery_gate: RwLock<()>,
    access: Arc<dyn EventAccess>,
    sender: Arc<dyn EventDelivery>,
    degraded: AtomicBool,
    wake: Notify,
}

pub fn state_key(config: &SlackEventsConfig) -> anyhow::Result<String> {
    // A temporary filter exception does not change subscription identity.
    let mut identity = config.clone();
    identity.owner_proof_until = None;
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&identity)?)))
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn iso_time(seconds: u64) -> Result<String, EventError> {
    chrono::DateTime::from_timestamp(
        i64::try_from(seconds).map_err(|_| invalid("invalid timestamp"))?,
        0,
    )
    .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    .ok_or_else(|| invalid("invalid timestamp"))
}

impl SlackEvents {
    pub fn open(
        config: SlackEventsConfig,
        source_secret: crate::types::SecretString,
        dir: &Path,
        access: Arc<dyn EventAccess>,
        sender: Arc<dyn EventDelivery>,
    ) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(
            config.validate().is_empty(),
            "invalid Slack event configuration"
        );
        anyhow::ensure!(
            !source_secret.as_str().is_empty(),
            "Slack event signing secret is required"
        );
        crate::fs_perm::ensure_dir_0700(dir)?;
        check_private(dir, true)?;
        let path = dir.join("state.json");
        let lock_path = dir.join("state.lock");
        let lock = private_open(&lock_path, false)?;
        check_private(&lock_path, false)?;
        FileExt::try_lock(&lock)
            .map_err(|_| anyhow::anyhow!("Slack event state already in use"))?;
        let state = if path.exists() {
            check_private(&path, false)?;
            let bytes = std::fs::read(&path)?;
            anyhow::ensure!(
                bytes.len() <= 8 * 1024 * 1024,
                "Slack event state exceeds limit"
            );
            let value: EventState = serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("invalid Slack event state"))?;
            anyhow::ensure!(
                value.version == 2
                    && value.queue.len() <= MAX_QUEUE
                    && value.seen.len() <= MAX_SEEN
                    && value.threads.len() <= MAX_SEEN
                    && value
                        .queue
                        .iter()
                        .all(|pending| valid_public_channel(&pending.channel_id)
                            && pending.body.len() <= MAX_BODY
                            && pending.attempts < MAX_ATTEMPTS),
                "unsupported Slack event state"
            );
            value
        } else {
            EventState::default()
        };
        Ok(Arc::new(Self {
            config,
            source_secret,
            path,
            _lock: lock,
            state: Mutex::new(state),
            delivery_gate: RwLock::new(()),
            access,
            sender,
            degraded: AtomicBool::new(false),
            wake: Notify::new(),
        }))
    }

    /// Static capability and catalog discovery lets the selected client request consent;
    /// subscription and delivery access require its separately approved scope.
    pub fn discoverable_to(&self, client_id: &str) -> bool {
        !self.degraded.load(Ordering::Acquire)
            && self.access.enabled()
            && client_id == self.config.subscriber_client_id
    }

    pub fn available_to(&self, client_id: &str, scopes: &[String]) -> bool {
        self.discoverable_to(client_id) && scopes.iter().any(|s| s == EVENT_SCOPE)
    }

    pub fn catalog(&self) -> Value {
        json!({"events": [{"name": EVENT_NAME,
            "description": "Coworker mentions of the existing dot, and human follow-ups in participating threads across accessible public channels delivered by Slack. Owner and bot messages are excluded, apart from an explicitly enabled, expiring exact-marker owner proof.",
            "delivery": ["webhook"],
            "inputSchema": {"type":"object", "properties":{"scope":{"type":"string","enum":["accessible_public_channels"]}},"required":["scope"],"additionalProperties":false},
            "payloadSchema": {"type":"object", "properties": {
                "team_id":{"type":"string"},"channel_id":{"type":"string"},"user_id":{"type":"string"},
                "message_ts":{"type":"string"},"thread_ts":{"type":"string"},"text":{"type":"string"},
                "url":{"type":"string"},"reason":{"type":"string","enum":["mention","thread_reply"]}},
                "required":["team_id","channel_id","user_id","message_ts","thread_ts","text","url","reason"],"additionalProperties":false}}]})
    }

    fn identity(&self, params: &Value) -> Result<(String, String), EventError> {
        let object = params
            .as_object()
            .ok_or_else(|| invalid("event params must be an object"))?;
        if object.keys().any(|k| {
            !["name", "arguments", "delivery", "ttlMs", "cursor", "_meta"].contains(&k.as_str())
        }) {
            return Err(invalid("unknown event parameter"));
        }
        if params["name"] != EVENT_NAME
            || params["arguments"] != json!({"scope":"accessible_public_channels"})
        {
            return Err(invalid("unsupported event or scope"));
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
        delivery::validate_url(url).map_err(callback_error)?;
        // Fixed one-field arguments are canonical by construction. Include the
        // configured source identity so changed source config cannot reuse old grants.
        let key = serde_json::to_vec(
            &json!([self.config, EVENT_NAME, url, {"scope":"accessible_public_channels"}]),
        )
        .map_err(|_| unavailable())?;
        Ok((
            format!("sub_{}", hex::encode(Sha256::digest(key))),
            url.to_owned(),
        ))
    }

    pub async fn subscribe(&self, params: &Value) -> Result<Value, EventError> {
        let (id, url) = self.identity(params)?;
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
        // Always grant a finite lifetime, including ttlMs:null. Round down so we
        // never grant more than a requested millisecond duration.
        if ttl < 1000 {
            return Err(invalid("minimum supported ttlMs is 1000"));
        }
        let _gate = self.delivery_gate.write().await;
        self.require_access().await?;
        let current = self.state.lock().await.subscription.clone();
        if self.state.lock().await.source_revoked {
            return Err(denied());
        }
        if current
            .as_ref()
            .is_some_and(|s| s.id != id && s.expires > now())
        {
            return Err(invalid("one callback subscription is permitted"));
        }
        let verified = current.as_ref().is_some_and(|s| {
            s.id == id
                && bool::from(s.secret.as_str().as_bytes().ct_eq(secret.as_bytes()))
                && s.verified_until > now()
        });
        if !verified {
            let challenge = uuid::Uuid::new_v4().to_string();
            let body = serde_json::to_vec(&json!({"type":"verification","challenge":challenge}))
                .map_err(|_| unavailable())?;
            let verification_id = format!("msg_verification_{}", uuid::Uuid::new_v4());
            let headers = signed_headers(&id, &verification_id, secret, None, &body, now())?;
            let response = self
                .sender
                .post(&url, headers, body)
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
        self.require_access().await?;
        let timestamp = now();
        let mut state = self.state.lock().await;
        if state.source_revoked {
            return Err(denied());
        }
        let mut next = state.clone();
        let old_secret = current
            .as_ref()
            .filter(|s| {
                s.id == id
                    && s.expires > timestamp
                    && !bool::from(s.secret.as_str().as_bytes().ct_eq(secret.as_bytes()))
            })
            .map(|s| s.secret.clone())
            .or_else(|| {
                current
                    .as_ref()
                    .filter(|s| s.id == id && s.old_secret_until > timestamp)
                    .and_then(|s| s.old_secret.clone())
            });
        let old_secret_until = current
            .as_ref()
            .filter(|s| bool::from(s.secret.as_str().as_bytes().ct_eq(secret.as_bytes())))
            .map_or(timestamp + 300, |s| s.old_secret_until);
        if current
            .as_ref()
            .is_some_and(|s| s.id != id || s.expires <= timestamp)
        {
            next.queue.clear();
            next.threads.clear();
        }
        next.subscription = Some(Subscription {
            id: id.clone(),
            url,
            secret: secret.to_owned().into(),
            old_secret,
            old_secret_until,
            expires: timestamp + ttl / 1000,
            verified_until: timestamp + 300,
        });
        self.commit(&mut state, next)?;
        self.wake.notify_one();
        Ok(
            json!({"id":id,"refreshBefore":iso_time(timestamp + ttl / 1000)?,"cursor":null,"truncated":false}),
        )
    }

    pub async fn unsubscribe(&self, params: &Value) -> Result<Value, EventError> {
        let (id, _) = self.identity(params)?;
        let _gate = self.delivery_gate.write().await;
        // HTTP has authenticated this exact configured client. Unsubscribe must
        // still work when Slack is unavailable or channel access has been revoked.
        let mut state = self.state.lock().await;
        let mut next = state.clone();
        if next.subscription.as_ref().is_some_and(|s| s.id == id) {
            next.subscription = None;
            next.queue.clear();
            next.threads.clear();
            self.commit(&mut state, next)?;
        }
        Ok(json!({}))
    }

    async fn require_access(&self) -> Result<(), EventError> {
        if self.degraded.load(Ordering::Acquire) || !self.access.enabled() {
            return Err(denied());
        }
        match self.access.check(None).await {
            Access::Allowed => Ok(()),
            Access::Denied => Err(denied()),
            Access::Unavailable => Err(unavailable()),
        }
    }

    /// Returns a URL-verification response or an empty event acknowledgement.
    /// A message is acknowledged only after its dedupe/queue state is durable.
    pub async fn ingest(&self, headers: &HeaderMap, body: &[u8]) -> Result<Value, EventError> {
        if self.degraded.load(Ordering::Acquire) || !self.access.enabled() {
            return Err(unavailable());
        }
        if body.len() > MAX_BODY {
            return Err(invalid("Slack event exceeds size limit"));
        }
        verify_slack(&self.source_secret, headers, body, now())?;
        let envelope: Value =
            serde_json::from_slice(body).map_err(|_| invalid("invalid Slack event JSON"))?;
        if envelope["type"] == "url_verification" {
            let challenge = envelope["challenge"]
                .as_str()
                .filter(|c| c.len() <= 1024)
                .ok_or_else(|| invalid("invalid Slack challenge"))?;
            return Ok(json!({"challenge":challenge}));
        }
        if envelope["api_app_id"] != self.config.app_id
            || envelope["team_id"] != self.config.team_id
        {
            return Err(denied());
        }
        if envelope["type"] != "event_callback" {
            return Err(invalid("unsupported Slack envelope"));
        }
        let event = &envelope["event"];
        let revocation = event["type"] == "app_uninstalled"
            || (event["type"] == "tokens_revoked"
                && event["tokens"]["oauth"]
                    .as_array()
                    .is_some_and(|users| users.iter().any(|u| u == &self.config.owner_user_id)));
        // Stop revocations even when Slack omits authorizations on lifecycle events.
        // Wait for any in-flight callback before acknowledging the stop.
        let _revocation_gate = if revocation {
            Some(self.delivery_gate.write().await)
        } else {
            None
        };
        // User-authorized source only, never borrow another app/bot installation.
        let authorized = envelope["authorizations"].as_array().is_some_and(|items| {
            items.iter().any(|a| {
                a["team_id"] == self.config.team_id
                    && a["user_id"] == self.config.owner_user_id
                    && a["is_bot"] == false
            })
        });
        if !authorized && !revocation {
            return Err(denied());
        }
        let event_id = envelope["event_id"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 128)
            .ok_or_else(|| invalid("Slack event ID is required"))?;
        let timestamp = now();
        let mut state = self.state.lock().await;
        let mut next = state.clone();
        next.seen.retain(|_, time| *time + 7 * DAY > timestamp);
        next.threads.retain(|_, time| *time + 7 * DAY > timestamp);
        if next.seen.contains_key(event_id) {
            return Ok(json!({}));
        }
        if !revocation && next.seen.len() >= MAX_SEEN {
            return Err(unavailable());
        }
        if revocation {
            next.seen.clear();
            next.source_revoked = true;
            next.subscription = None;
            next.queue.clear();
            next.threads.clear();
        } else if !next.source_revoked
            && next
                .subscription
                .as_ref()
                .is_some_and(|s| s.expires > timestamp)
            && event["type"] == "message"
            && event["channel"].as_str().is_some_and(valid_public_channel)
            && event["channel_type"] == "channel"
            && event["user"] == self.config.dot_user_id
            && event.get("subtype").is_none_or(|s| s == "bot_message")
        {
            // Observe the existing dot's participation, but never forward its
            // messages back into the dot. This also covers owner-started threads.
            if let Some(root) = event["thread_ts"]
                .as_str()
                .or_else(|| event["ts"].as_str())
                .filter(|root| valid_slack_ts(root))
            {
                let thread_key = format!("{}:{root}", event["channel"].as_str().unwrap());
                if next.threads.len() >= MAX_SEEN && !next.threads.contains_key(&thread_key) {
                    return Err(unavailable());
                }
                next.threads.insert(thread_key, timestamp);
            }
        } else if !next.source_revoked
            && next
                .subscription
                .as_ref()
                .is_some_and(|s| s.expires > timestamp)
            && event["type"] == "message"
            && event["channel"].as_str().is_some_and(valid_public_channel)
            && event["channel_type"] == "channel"
            && event.get("subtype").is_none()
            && event.get("bot_id").is_none()
            && event.get("app_id").is_none()
        {
            let user = event["user"]
                .as_str()
                .filter(|u| u.starts_with('U'))
                .unwrap_or("");
            let ts = event["ts"]
                .as_str()
                .filter(|t| valid_slack_ts(t))
                .unwrap_or("");
            let root = event["thread_ts"].as_str().unwrap_or(ts);
            let text = event["text"].as_str().unwrap_or("");
            let owner_proof = user == self.config.owner_user_id
                && self
                    .config
                    .owner_proof_until
                    .is_some_and(|until| timestamp < until)
                && text.trim() == format!("<@{}> {OWNER_PROOF_MARKER}", self.config.dot_user_id);
            if !user.is_empty()
                && user != self.config.dot_user_id
                && (user != self.config.owner_user_id || owner_proof)
                && !ts.is_empty()
                && valid_slack_ts(root)
            {
                let mention = text.contains(&format!("<@{}>", self.config.dot_user_id));
                let channel = event["channel"].as_str().unwrap();
                let thread_key = format!("{channel}:{root}");
                let reply = root != ts && next.threads.contains_key(&thread_key);
                if mention || reply {
                    if next.queue.len() >= MAX_QUEUE
                        || (next.threads.len() >= MAX_SEEN
                            && !next.threads.contains_key(&thread_key))
                    {
                        return Err(unavailable());
                    }
                    let occurrence = ts
                        .split('.')
                        .next()
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(|| invalid("invalid Slack timestamp"))?;
                    let body = serde_json::to_vec(&json!({"eventId":event_id,"name":EVENT_NAME,"timestamp":iso_time(occurrence)?,"cursor":null,
                        "data":{"team_id":self.config.team_id,"channel_id":channel,"user_id":user,
                            "message_ts":ts,"thread_ts":root,"text":text,"reason":if mention {"mention"} else {"thread_reply"},
                            "url":format!("https://app.slack.com/archives/{}/p{}",channel,ts.replace('.',""))}})).map_err(|_| unavailable())?;
                    if body.len() > MAX_BODY {
                        return Err(invalid("event payload exceeds size limit"));
                    }
                    if !owner_proof {
                        next.threads.insert(thread_key, timestamp);
                    }
                    next.queue.push(Pending {
                        event_id: event_id.to_owned(),
                        channel_id: channel.to_owned(),
                        body,
                        attempts: 0,
                        next_at: timestamp,
                    });
                }
            }
        }
        if !revocation && next.queue.len() == state.queue.len() && next.threads == state.threads {
            return Ok(json!({}));
        }
        next.seen.insert(event_id.to_owned(), timestamp);
        self.commit(&mut state, next)?;
        self.wake.notify_one();
        Ok(json!({}))
    }

    fn commit(&self, state: &mut EventState, next: EventState) -> Result<(), EventError> {
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

    pub fn spawn(self: &Arc<Self>, cancel: CancellationToken) {
        let events = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = async {
                        if let Err(error) = events.deliver_one().await {
                            tracing::warn!(code = error.code, "Slack event delivery paused");
                        }
                        tokio::select! { _ = events.wake.notified() => {}, _ = tokio::time::sleep(Duration::from_secs(1)) => {} }
                    } => {}
                }
            }
        });
    }

    async fn deliver_one(&self) -> Result<(), EventError> {
        let _gate = self.delivery_gate.read().await;
        let timestamp = now();
        let (subscription, pending) = {
            let mut state = self.state.lock().await;
            if state
                .subscription
                .as_ref()
                .is_some_and(|s| s.expires <= timestamp)
            {
                let mut next = state.clone();
                next.subscription = None;
                next.queue.clear();
                next.threads.clear();
                self.commit(&mut state, next)?;
            }
            let Some(subscription) = state.subscription.clone() else {
                return Ok(());
            };
            let Some(pending) = state.queue.iter().find(|p| p.next_at <= timestamp).cloned() else {
                return Ok(());
            };
            (subscription, pending)
        };
        match self.access.check(None).await {
            Access::Denied => {
                let mut state = self.state.lock().await;
                let mut next = state.clone();
                next.subscription = None;
                next.queue.clear();
                next.threads.clear();
                return self.commit(&mut state, next);
            }
            Access::Unavailable => return Err(unavailable()),
            Access::Allowed => {}
        }
        match self.access.check(Some(&pending.channel_id)).await {
            Access::Allowed => {}
            Access::Denied => {
                let mut state = self.state.lock().await;
                let mut next = state.clone();
                next.queue.retain(|p| p.channel_id != pending.channel_id);
                let prefix = format!("{}:", pending.channel_id);
                next.threads.retain(|key, _| !key.starts_with(&prefix));
                return self.commit(&mut state, next);
            }
            Access::Unavailable => {
                let mut state = self.state.lock().await;
                let mut next = state.clone();
                if pending.attempts + 1 >= MAX_ATTEMPTS {
                    next.queue.retain(|p| p.event_id != pending.event_id);
                } else if let Some(job) = next
                    .queue
                    .iter_mut()
                    .find(|p| p.event_id == pending.event_id)
                {
                    job.attempts += 1;
                    job.next_at = timestamp + (1_u64 << job.attempts).min(300);
                }
                return self.commit(&mut state, next);
            }
        }
        if self.degraded.load(Ordering::Acquire) || !self.access.enabled() {
            return Err(denied());
        }
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
        let result = self
            .sender
            .post(&subscription.url, headers, pending.body.clone())
            .await;
        let status = result.as_ref().ok().map(|r| r.status);
        let mut state = self.state.lock().await;
        let mut next = state.clone();
        let accepted = status.is_some_and(|s| (200..300).contains(&s));
        let permanent = status.is_some_and(|s| (400..500).contains(&s) && ![408, 429].contains(&s));
        if accepted || permanent || pending.attempts + 1 >= MAX_ATTEMPTS {
            next.queue.retain(|p| p.event_id != pending.event_id);
            if status == Some(410) {
                next.subscription = None;
                next.queue.clear();
                next.threads.clear();
            }
            if !accepted {
                tracing::warn!(status, "Slack event delivery exhausted or rejected");
            }
        } else if let Some(job) = next
            .queue
            .iter_mut()
            .find(|p| p.event_id == pending.event_id)
        {
            job.attempts += 1;
            job.next_at = now() + (1_u64 << job.attempts).min(300);
        }
        self.commit(&mut state, next)
    }
}

fn valid_public_channel(value: &str) -> bool {
    value.starts_with('C')
        && value.len() > 1
        && value
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

fn valid_slack_ts(value: &str) -> bool {
    let Some((seconds, fraction)) = value.split_once('.') else {
        return false;
    };
    !seconds.is_empty()
        && seconds.len() <= 12
        && fraction.len() == 6
        && seconds
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
}
fn signing_key(secret: &str) -> Result<Vec<u8>, EventError> {
    let key = secret
        .strip_prefix("whsec_")
        .and_then(|s| STANDARD.decode(s).ok())
        .ok_or_else(|| invalid("invalid webhook signing secret"))?;
    if !(24..=64).contains(&key.len()) {
        return Err(invalid("invalid webhook signing secret length"));
    }
    Ok(key)
}
fn signature(key: &[u8], body: &[u8]) -> Vec<u8> {
    let mut hmac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    hmac.update(body);
    hmac.finalize().into_bytes().to_vec()
}
fn signed_headers(
    subscription: &str,
    event_id: &str,
    secret: &str,
    old: Option<&str>,
    body: &[u8],
    timestamp: u64,
) -> Result<HeaderMap, EventError> {
    let mut signing = format!("{event_id}.{timestamp}.").into_bytes();
    signing.extend_from_slice(body);
    let mut signatures = format!(
        "v1,{}",
        STANDARD.encode(signature(&signing_key(secret)?, &signing))
    );
    if let Some(old) = old {
        signatures.push_str(&format!(
            " v1,{}",
            STANDARD.encode(signature(&signing_key(old)?, &signing))
        ));
    }
    let mut headers = HeaderMap::new();
    for (name, value) in [
        ("content-type", "application/json".to_owned()),
        ("webhook-id", event_id.to_owned()),
        ("webhook-timestamp", timestamp.to_string()),
        ("webhook-signature", signatures),
        ("x-mcp-subscription-id", subscription.to_owned()),
    ] {
        headers.insert(
            axum::http::header::HeaderName::from_static(name),
            value
                .parse()
                .map_err(|_| invalid("invalid webhook header"))?,
        );
    }
    Ok(headers)
}
fn verify_slack(
    secret: &crate::types::SecretString,
    headers: &HeaderMap,
    body: &[u8],
    timestamp: u64,
) -> Result<(), EventError> {
    let sent = headers
        .get("x-slack-request-timestamp")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(denied)?;
    let seconds = sent.parse::<u64>().map_err(|_| denied())?;
    if timestamp.abs_diff(seconds) > 300 {
        return Err(denied());
    }
    let supplied = headers
        .get("x-slack-signature")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("v0="))
        .and_then(|s| hex::decode(s).ok())
        .ok_or_else(denied)?;
    let mut signed = format!("v0:{sent}:").into_bytes();
    signed.extend_from_slice(body);
    if !bool::from(signature(secret.as_str().as_bytes(), &signed).ct_eq(&supplied)) {
        return Err(denied());
    }
    Ok(())
}
fn private_open(path: &Path, exclusive: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).read(true);
    if exclusive {
        options.create_new(true);
    } else {
        options.create(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}
fn check_private(path: &Path, directory: bool) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        !metadata.file_type().is_symlink() && metadata.is_dir() == directory,
        "unsafe Slack event state path"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        anyhow::ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "unsafe Slack event state permissions"
        );
    }
    Ok(())
}
