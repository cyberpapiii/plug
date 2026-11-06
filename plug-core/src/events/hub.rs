//! The subscription engine every event source shares: the state file,
//! subscribing and refreshing, and webhook delivery with retries. A source
//! says what its events are, who may have them, and when one happens.
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::http::HeaderMap;
use fs4::FileExt;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, MutexGuard, Notify, RwLock, RwLockWriteGuard};
use tokio_util::sync::CancellationToken;

use super::delivery::{signed_headers, signing_key};
use super::{
    DAY, EventDelivery, EventError, EventSource, MAX_BODY, callback_error, denied, invalid,
    iso_time, now, unavailable, validate_url,
};
use crate::types::SecretString;

const MAX_STATE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_QUEUE: usize = 128;
/// Sends of one event before it is dropped.
pub(crate) const MAX_ATTEMPTS: u32 = 8;
/// The longest wait between two sends of one event.
const MAX_BACKOFF_SECS: u64 = 300;
/// How long a replaced secret keeps signing beside the new one.
const ROTATION_SECS: u64 = 300;
/// How long a passed callback challenge spares a refresh from another.
const VERIFIED_SECS: u64 = 300;
/// How often delivery looks for due events when nothing wakes it.
const DELIVERY_TICK: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Allowed,
    Denied,
    /// Could not tell right now. Try again later.
    Unavailable,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub(crate) id: String,
    pub(crate) client_id: String,
    pub(crate) event: String,
    url: String,
    secret: SecretString,
    old_secret: Option<SecretString>,
    old_secret_until: u64,
    pub(crate) expires: u64,
    verified_until: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Pending {
    pub(crate) subscription_id: String,
    pub(crate) event_id: String,
    /// Where in the source the event came from, such as a Slack channel.
    /// Access to it is asked again before each send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) origin: Option<String>,
    pub(crate) body: Vec<u8>,
    pub(crate) attempts: u32,
    pub(crate) next_at: u64,
}

impl Pending {
    pub(crate) fn new(
        subscription_id: String,
        event_id: String,
        origin: Option<String>,
        body: Vec<u8>,
        at: u64,
    ) -> Self {
        Self {
            subscription_id,
            event_id,
            origin,
            body,
            attempts: 0,
            next_at: at,
        }
    }

    /// The MCP event envelope for `data`, or `None` when it is larger than
    /// an event may be.
    pub(crate) fn body(
        event_id: &str,
        name: &str,
        at: u64,
        data: Value,
    ) -> Result<Option<Vec<u8>>, EventError> {
        let body = serde_json::to_vec(&json!({
            "eventId": event_id,
            "name": name,
            "timestamp": iso_time(at)?,
            "cursor": null,
            "data": data,
        }))
        .map_err(|_| unavailable())?;
        Ok((body.len() <= MAX_BODY).then_some(body))
    }

    fn is(&self, other: &Pending) -> bool {
        self.subscription_id == other.subscription_id && self.event_id == other.event_id
    }
}

/// What a source keeps in the state file beside the subscriptions.
pub trait SourceState:
    Clone + Default + Serialize + DeserializeOwned + Send + Sync + 'static
{
    /// Forget what belonged to a subscription that ended.
    fn forget(&mut self, _subscription: &Subscription) {}
    /// Forget what came from `origin` once access to it is gone.
    fn forget_origin(&mut self, _origin: &str) {}
}

#[derive(Clone, Serialize, Deserialize)]
pub struct State<X> {
    pub(crate) version: u8,
    pub(crate) subscriptions: Vec<Subscription>,
    pub(crate) queue: Vec<Pending>,
    #[serde(flatten)]
    pub(crate) source: X,
}

impl<X: SourceState> State<X> {
    fn new(version: u8) -> Self {
        Self {
            version,
            subscriptions: Vec::new(),
            queue: Vec::new(),
            source: X::default(),
        }
    }

    pub(crate) fn live(&self, at: u64) -> impl Iterator<Item = &Subscription> {
        self.subscriptions.iter().filter(move |s| s.expires > at)
    }

    /// Queue `pending` unless the queue is full. Returns whether it was queued.
    pub(crate) fn enqueue(&mut self, pending: Pending) -> bool {
        if self.queue.len() >= MAX_QUEUE {
            return false;
        }
        self.queue.push(pending);
        true
    }

    /// End the subscriptions `gone` picks, with what is queued for them.
    /// Returns whether any ended.
    pub(crate) fn end_subscriptions(
        &mut self,
        mut gone: impl FnMut(&Subscription) -> bool,
    ) -> bool {
        let mut ended = Vec::new();
        self.subscriptions.retain(|s| {
            let keep = !gone(s);
            if !keep {
                ended.push(s.clone());
            }
            keep
        });
        for subscription in &ended {
            self.queue.retain(|p| p.subscription_id != subscription.id);
            self.source.forget(subscription);
        }
        !ended.is_empty()
    }

    /// Schedule another send of `pending` after a backoff, or drop it once it
    /// has had all its attempts. Returns whether it is still queued.
    fn retry(&mut self, pending: &Pending, at: u64) -> bool {
        if pending.attempts + 1 >= MAX_ATTEMPTS {
            self.queue.retain(|p| !p.is(pending));
            return false;
        }
        if let Some(job) = self.queue.iter_mut().find(|p| p.is(pending)) {
            job.attempts += 1;
            job.next_at = at + (1_u64 << job.attempts).min(MAX_BACKOFF_SECS);
        }
        true
    }
}

/// One kind of event, plugged into a `Hub`.
#[allow(
    clippy::double_must_use,
    reason = "async_trait generates redundant must_use on boxed futures"
)]
#[async_trait::async_trait]
pub trait Source: Send + Sync + Sized + 'static {
    type State: SourceState;
    /// The `version` its state file is written with.
    const STATE_VERSION: u8;
    /// How many live subscriptions it holds at once.
    const MAX_SUBSCRIPTIONS: usize;
    /// Its name in logs.
    const LABEL: &'static str;
    /// Where it takes events pushed to it from outside, if anywhere.
    const INGEST_PATH: Option<&'static str> = None;

    /// Read a state file written in an older layout.
    fn upgrade(&self, _version: u8, _bytes: &[u8]) -> Option<State<Self::State>> {
        None
    }
    /// Whether state read from disk is within the source's own bounds.
    fn fits(&self, _state: &State<Self::State>) -> bool {
        true
    }
    fn discoverable_to(&self, client_id: &str) -> bool;
    fn catalog(&self, client_id: &str) -> Vec<Value>;
    fn owns(&self, event: &str) -> bool;
    /// The event a subscribe request's `name` and `arguments` ask for.
    fn resolve(&self, name: &Value, arguments: &Value) -> Result<String, EventError>;
    /// The ID of `client_id`'s subscription to `event` at `url`. Subscribing
    /// again with the same three refreshes that subscription. Build it with
    /// `subscription_id`.
    fn subscription_id(
        &self,
        client_id: &str,
        event: &str,
        url: &str,
    ) -> Result<String, EventError>;
    /// Whether the source takes subscriptions in its present state.
    fn accepts(&self, _state: &Self::State) -> bool {
        true
    }
    /// Whether `client_id` may have `event` now. Asked when it subscribes
    /// and before each send.
    async fn permits(&self, client_id: &str, event: &str) -> Access;
    /// Whether events from `origin` may still be sent.
    async fn admits(&self, _origin: &str) -> Access {
        Access::Allowed
    }
    /// Drop source state that no longer applies. Returns whether it changed.
    fn tidy(&self, _state: &mut Self::State) -> bool {
        false
    }
    async fn ingest(
        _hub: &Hub<Self>,
        _headers: &HeaderMap,
        _body: &[u8],
    ) -> Result<Value, EventError> {
        Err(invalid("this event source takes nothing from outside"))
    }
    /// Start whatever makes the source's events, until `cancel` fires.
    fn start(_hub: &Arc<Hub<Self>>, _cancel: &CancellationToken) {}
}

/// A subscription ID derived from everything that identifies it.
pub(crate) fn subscription_id(identity: &Value) -> Result<String, EventError> {
    let key = serde_json::to_vec(identity).map_err(|_| unavailable())?;
    Ok(format!("sub_{}", hex::encode(Sha256::digest(key))))
}

/// Secrets and event bodies never implement Debug or enter diagnostics.
pub struct Hub<S: Source> {
    pub(super) source: S,
    pub(super) path: PathBuf,
    _lock: File,
    pub(super) state: Mutex<State<S::State>>,
    /// Lets unsubscribe and refresh wait out an in-flight send, without
    /// making ingest wait for network calls.
    delivery_gate: RwLock<()>,
    sender: Arc<dyn EventDelivery>,
    /// Set once the state file could not be written. Nothing more is
    /// accepted or sent until restart.
    pub(super) degraded: AtomicBool,
    wake: Notify,
}

struct Target {
    id: String,
    event: String,
    url: String,
}

impl<S: Source> Hub<S> {
    /// Open the state in `dir`, which only this process may then use.
    pub(crate) fn load(
        dir: &Path,
        source: S,
        sender: Arc<dyn EventDelivery>,
    ) -> anyhow::Result<Arc<Self>> {
        crate::fs_perm::ensure_dir_0700(dir)?;
        check_private(dir, true)?;
        let path = dir.join("state.json");
        let lock_path = dir.join("state.lock");
        let lock = {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .write(true)
                .read(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(&lock_path)?
        };
        check_private(&lock_path, false)?;
        FileExt::try_lock(&lock).map_err(|_| anyhow::anyhow!("event state already in use"))?;
        let state = if path.exists() {
            check_private(&path, false)?;
            let bytes = std::fs::read(&path)?;
            anyhow::ensure!(bytes.len() <= MAX_STATE_BYTES, "event state exceeds limit");
            #[derive(Deserialize)]
            struct Version {
                version: u8,
            }
            let version = serde_json::from_slice::<Version>(&bytes)
                .map_err(|_| anyhow::anyhow!("invalid event state"))?
                .version;
            let mut state: State<S::State> = if version == S::STATE_VERSION {
                serde_json::from_slice(&bytes)
                    .map_err(|_| anyhow::anyhow!("invalid event state"))?
            } else {
                source
                    .upgrade(version, &bytes)
                    .ok_or_else(|| anyhow::anyhow!("unsupported event state"))?
            };
            state.version = S::STATE_VERSION;
            anyhow::ensure!(
                state.queue.len() <= MAX_QUEUE
                    && state.subscriptions.len() <= S::MAX_SUBSCRIPTIONS
                    && state
                        .queue
                        .iter()
                        .all(|p| p.body.len() <= MAX_BODY && p.attempts < MAX_ATTEMPTS)
                    && source.fits(&state),
                "unsupported event state"
            );
            state
        } else {
            State::new(S::STATE_VERSION)
        };
        Ok(Arc::new(Self {
            source,
            path,
            _lock: lock,
            state: Mutex::new(state),
            delivery_gate: RwLock::new(()),
            sender,
            degraded: AtomicBool::new(false),
            wake: Notify::new(),
        }))
    }

    pub(crate) fn source(&self) -> &S {
        &self.source
    }

    pub(crate) async fn state(&self) -> MutexGuard<'_, State<S::State>> {
        self.state.lock().await
    }

    pub(crate) fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::Acquire)
    }

    /// Tell delivery an event was queued.
    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    /// Wait for an in-flight send to finish, and hold off the next one while
    /// the guard lives.
    pub(crate) async fn hold_delivery(&self) -> RwLockWriteGuard<'_, ()> {
        self.delivery_gate.write().await
    }

    /// Write `next` to disk, then make it the state. A failed write stops the
    /// hub taking changes until restart, so memory never runs ahead of disk.
    pub(crate) fn commit(
        &self,
        state: &mut State<S::State>,
        next: State<S::State>,
    ) -> Result<(), EventError> {
        if self.is_degraded() {
            return Err(unavailable());
        }
        let bytes = serde_json::to_vec(&next).map_err(|_| unavailable())?;
        if bytes.len() > MAX_STATE_BYTES {
            return Err(unavailable());
        }
        if let Err(error) = crate::fs_perm::write_private_atomic(&self.path, &bytes) {
            tracing::error!(source = S::LABEL, %error, "event state could not be saved; events stop until restart");
            self.degraded.store(true, Ordering::Release);
            return Err(unavailable());
        }
        *state = next;
        Ok(())
    }

    fn target(&self, client_id: &str, params: &Value) -> Result<Target, EventError> {
        let object = params
            .as_object()
            .ok_or_else(|| invalid("event params must be an object"))?;
        if object.keys().any(|k| {
            !["name", "arguments", "delivery", "ttlMs", "cursor", "_meta"].contains(&k.as_str())
        }) {
            return Err(invalid("unknown event parameter"));
        }
        let event = self.source.resolve(&params["name"], &params["arguments"])?;
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
        Ok(Target {
            id: self.source.subscription_id(client_id, &event, url)?,
            event,
            url: url.to_owned(),
        })
    }

    async fn require(&self, client_id: &str, event: &str) -> Result<(), EventError> {
        match self.source.permits(client_id, event).await {
            Access::Allowed => Ok(()),
            Access::Denied => Err(denied()),
            Access::Unavailable => Err(unavailable()),
        }
    }

    /// Post a challenge to the callback and require it echoed back.
    async fn verify_callback(&self, target: &Target, secret: &str) -> Result<(), EventError> {
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
        Ok(())
    }

    /// End expired subscriptions and those to events the source no longer
    /// makes, and let the source tidy its own state.
    pub(crate) async fn prune(&self) -> Result<(), EventError> {
        let _gate = self.delivery_gate.read().await;
        let timestamp = now();
        let mut state = self.state.lock().await;
        let mut next = state.clone();
        let mut changed =
            next.end_subscriptions(|s| s.expires <= timestamp || !self.source.owns(&s.event));
        let ids: HashSet<String> = next.subscriptions.iter().map(|s| s.id.clone()).collect();
        let queued = next.queue.len();
        next.queue.retain(|p| ids.contains(&p.subscription_id));
        changed |= next.queue.len() != queued;
        changed |= self.source.tidy(&mut next.source);
        if !changed {
            return Ok(());
        }
        self.commit(&mut state, next)
    }

    pub(crate) async fn deliver_one(&self) -> Result<(), EventError> {
        // Degrading logged once; nothing more is sent until restart.
        if self.is_degraded() {
            return Ok(());
        }
        let _gate = self.delivery_gate.read().await;
        let timestamp = now();
        let (subscription, pending) = {
            let mut state = self.state.lock().await;
            if state.subscriptions.iter().any(|s| s.expires <= timestamp) {
                let mut next = state.clone();
                next.end_subscriptions(|s| s.expires <= timestamp);
                self.commit(&mut state, next)?;
            }
            let Some(pending) = state.queue.iter().find(|p| p.next_at <= timestamp).cloned() else {
                return Ok(());
            };
            let subscription = state
                .subscriptions
                .iter()
                .find(|s| s.id == pending.subscription_id)
                .cloned();
            (subscription, pending)
        };
        let access = match &subscription {
            Some(s) => self.source.permits(&s.client_id, &s.event).await,
            None => Access::Denied,
        };
        let subscription = match (access, subscription) {
            (Access::Allowed, Some(subscription)) => subscription,
            (Access::Unavailable, _) => return self.retry_later(&pending, timestamp).await,
            _ => {
                let mut state = self.state.lock().await;
                let mut next = state.clone();
                next.end_subscriptions(|s| s.id == pending.subscription_id);
                next.queue
                    .retain(|p| p.subscription_id != pending.subscription_id);
                return self.commit(&mut state, next);
            }
        };
        if let Some(origin) = &pending.origin {
            match self.source.admits(origin).await {
                Access::Allowed => {}
                Access::Denied => {
                    let mut state = self.state.lock().await;
                    let mut next = state.clone();
                    next.queue
                        .retain(|p| p.origin.as_deref() != Some(origin.as_str()));
                    next.source.forget_origin(origin);
                    return self.commit(&mut state, next);
                }
                Access::Unavailable => return self.retry_later(&pending, timestamp).await,
            }
        }
        let old = subscription
            .old_secret
            .as_ref()
            .filter(|_| subscription.old_secret_until > timestamp)
            .map(SecretString::as_str);
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
        let error = result.as_ref().err().copied();
        let accepted = status.is_some_and(|s| (200..300).contains(&s));
        let rejected = status.is_some_and(|s| (400..500).contains(&s) && ![408, 429].contains(&s));
        let mut state = self.state.lock().await;
        let mut next = state.clone();
        let given_up = if accepted || rejected {
            next.queue.retain(|p| !p.is(&pending));
            rejected
        } else {
            !next.retry(&pending, now())
        };
        // 410 Gone: the receiver says the subscription itself is over.
        if status == Some(410) {
            next.end_subscriptions(|s| s.id == subscription.id);
        }
        if given_up {
            tracing::warn!(
                source = S::LABEL,
                status,
                error,
                "event delivery exhausted or rejected"
            );
        } else if !accepted {
            tracing::debug!(
                source = S::LABEL,
                status,
                error,
                "event delivery will retry"
            );
        }
        self.commit(&mut state, next)
    }

    /// Access could not be told: send `pending` again after its backoff.
    async fn retry_later(&self, pending: &Pending, at: u64) -> Result<(), EventError> {
        let mut state = self.state.lock().await;
        let mut next = state.clone();
        next.retry(pending, at);
        self.commit(&mut state, next)
    }

    /// Run delivery, and whatever makes the source's events, until `cancel`
    /// fires.
    pub fn spawn(self: &Arc<Self>, cancel: CancellationToken) {
        S::start(self, &cancel);
        let hub = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = async {
                        if let Err(error) = hub.deliver_one().await {
                            tracing::warn!(source = S::LABEL, code = error.code, "event delivery paused");
                        }
                        tokio::select! {
                            _ = hub.wake.notified() => {},
                            _ = tokio::time::sleep(DELIVERY_TICK) => {},
                        }
                    } => {}
                }
            }
        });
    }
}

#[async_trait::async_trait]
impl<S: Source> EventSource for Hub<S> {
    fn discoverable_to(&self, client_id: &str) -> bool {
        !self.is_degraded() && self.source.discoverable_to(client_id)
    }

    fn catalog(&self, client_id: &str) -> Vec<Value> {
        if self.is_degraded() {
            return Vec::new();
        }
        self.source.catalog(client_id)
    }

    fn owns(&self, event: &str) -> bool {
        !self.is_degraded() && self.source.owns(event)
    }

    async fn subscribe(&self, client_id: &str, params: &Value) -> Result<Value, EventError> {
        if self.is_degraded() {
            return Err(unavailable());
        }
        let target = self.target(client_id, params)?;
        if params.get("cursor").is_some_and(|v| !v.is_null()) {
            return Err(invalid("this event does not support replay"));
        }
        let secret = params["delivery"]["secret"]
            .as_str()
            .ok_or_else(|| invalid("signing secret is required"))?;
        signing_key(secret)?;
        let ttl = ttl_secs(params.get("ttlMs"))?;
        let _gate = self.delivery_gate.write().await;
        self.require(client_id, &target.event).await?;
        let current = {
            let state = self.state.lock().await;
            if !self.source.accepts(&state.source) {
                return Err(denied());
            }
            if state.live(now()).filter(|s| s.id != target.id).count() >= S::MAX_SUBSCRIPTIONS {
                return Err(invalid("too many event subscriptions"));
            }
            state
                .subscriptions
                .iter()
                .find(|s| s.id == target.id)
                .cloned()
        };
        let verified = current
            .as_ref()
            .is_some_and(|s| same_secret(s, secret) && s.verified_until > now());
        if !verified {
            self.verify_callback(&target, secret).await?;
            // The challenge can be slow; access may have changed meanwhile.
            self.require(client_id, &target.event).await?;
        }
        let timestamp = now();
        let live = current.as_ref().filter(|s| s.expires > timestamp);
        // A new secret on refresh rotates it: the old one keeps signing too
        // for a while, so the receiver can switch over.
        let old_secret = live
            .filter(|s| !same_secret(s, secret))
            .map(|s| s.secret.clone())
            .or_else(|| {
                live.filter(|s| s.old_secret_until > timestamp)
                    .and_then(|s| s.old_secret.clone())
            });
        let old_secret_until = live
            .filter(|s| same_secret(s, secret))
            .map_or(timestamp + ROTATION_SECS, |s| s.old_secret_until);
        let mut state = self.state.lock().await;
        if !self.source.accepts(&state.source) {
            return Err(denied());
        }
        let mut next = state.clone();
        next.end_subscriptions(|s| s.expires <= timestamp);
        next.subscriptions.retain(|s| s.id != target.id);
        next.subscriptions.push(Subscription {
            id: target.id.clone(),
            client_id: client_id.to_owned(),
            event: target.event,
            url: target.url,
            secret: secret.to_owned().into(),
            old_secret,
            old_secret_until,
            expires: timestamp + ttl,
            verified_until: timestamp + VERIFIED_SECS,
        });
        self.commit(&mut state, next)?;
        self.wake();
        Ok(json!({
            "id": target.id,
            "refreshBefore": iso_time(timestamp + ttl)?,
            "cursor": null,
            "truncated": false,
        }))
    }

    async fn unsubscribe(&self, client_id: &str, params: &Value) -> Result<Value, EventError> {
        let target = self.target(client_id, params)?;
        let _gate = self.delivery_gate.write().await;
        // No access check: unsubscribing must work after access is lost.
        let mut state = self.state.lock().await;
        let mut next = state.clone();
        if next.end_subscriptions(|s| s.id == target.id) {
            self.commit(&mut state, next)?;
        }
        Ok(json!({}))
    }

    fn ingest_path(&self) -> Option<&'static str> {
        S::INGEST_PATH
    }

    async fn ingest(&self, headers: &HeaderMap, body: &[u8]) -> Result<Value, EventError> {
        S::ingest(self, headers, body).await
    }
}

/// The lifetime to grant, in whole seconds. Always finite, including for
/// `ttlMs: null`, and rounded down so it is never more than was asked.
fn ttl_secs(ttl_ms: Option<&Value>) -> Result<u64, EventError> {
    let ms = match ttl_ms {
        None | Some(Value::Null) => DAY * 1000,
        Some(v) => v
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or_else(|| invalid("ttlMs must be positive or null"))?
            .min(DAY * 1000),
    };
    if ms < 1000 {
        return Err(invalid("minimum supported ttlMs is 1000"));
    }
    Ok(ms / 1000)
}

fn same_secret(subscription: &Subscription, secret: &str) -> bool {
    bool::from(
        subscription
            .secret
            .as_str()
            .as_bytes()
            .ct_eq(secret.as_bytes()),
    )
}

fn check_private(path: &Path, directory: bool) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        !metadata.file_type().is_symlink() && metadata.is_dir() == directory,
        "unsafe event state path"
    );
    anyhow::ensure!(
        metadata.permissions().mode() & 0o077 == 0,
        "unsafe event state permissions"
    );
    Ok(())
}
