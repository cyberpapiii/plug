//! Opt-in one-public-channel source for OpenAI's MCP Events webhook contract.
//! Slack posts its Events API to `SOURCE_PATH`; matching messages become
//! `slack.ditto_message` events in the shared `crate::events` hub. This does
//! not forward resource subscriptions or replace the native dot app.
mod access;
pub mod credentials;
pub use access::RuntimeAccess;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::events::{
    Access, DAY, EventDelivery, EventError, Hub, MAX_BODY, MAX_QUEUE, Pending, Source, SourceState,
    State, Subscription, denied, hmac_sha256, invalid, iso_time, now, unavailable,
};

pub use crate::config::{SLACK_EVENT_NAME as EVENT_NAME, SlackEventsConfig};
pub const SOURCE_PATH: &str = "/events/slack";
const OWNER_PROOF_MARKER: &str = "PLUG_EVENTS_PROOF_20261003";
const MAX_SEEN: usize = 10_000;
/// How long a Slack event ID, and a thread the dot is in, are remembered.
const REMEMBER_SECS: u64 = 7 * DAY;
/// How far a signed Slack request's timestamp may be from now (Slack's
/// replay guidance).
const SIGNATURE_WINDOW_SECS: u64 = 300;

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

/// Secrets and message bodies never implement Debug or enter diagnostics.
pub struct SlackSource {
    pub config: SlackEventsConfig,
    source_secret: crate::types::SecretString,
    access: Arc<dyn EventAccess>,
}

pub type SlackEvents = Hub<SlackSource>;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SlackState {
    /// Slack event IDs already handled, so Slack's retries are not resent.
    pub(crate) seen: BTreeMap<String, u64>,
    /// `<channel>:<thread ts>` of threads the dot is in, whose replies count.
    pub(crate) threads: BTreeMap<String, u64>,
    /// Slack said the app was uninstalled or the owner's token revoked.
    pub(crate) source_revoked: bool,
}

impl SourceState for SlackState {
    fn forget(&mut self, _subscription: &Subscription) {
        self.threads.clear();
    }

    fn forget_origin(&mut self, channel: &str) {
        let prefix = format!("{channel}:");
        self.threads.retain(|key, _| !key.starts_with(&prefix));
    }
}

pub fn state_key(config: &SlackEventsConfig) -> anyhow::Result<String> {
    // A temporary filter exception does not change subscription identity.
    let mut identity = config.clone();
    identity.owner_proof_until = None;
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&identity)?)))
}

fn scope_arguments() -> Value {
    json!({"scope":"accessible_public_channels"})
}

impl Hub<SlackSource> {
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
        let source = SlackSource {
            config,
            source_secret,
            access,
        };
        Hub::load(dir, source, sender)
    }
}

#[async_trait::async_trait]
impl Source for SlackSource {
    type State = SlackState;
    const STATE_VERSION: u8 = 3;
    const MAX_SUBSCRIPTIONS: usize = 1;
    const LABEL: &'static str = "slack";
    const INGEST_PATH: Option<&'static str> = Some(SOURCE_PATH);

    /// Version 2 held one subscription and tagged each queued event with its
    /// channel. Installs that ran 0.9 to 0.11 have one on disk.
    fn upgrade(&self, version: u8, bytes: &[u8]) -> Option<State<SlackState>> {
        if version != 2 {
            return None;
        }
        let old: Value = serde_json::from_slice(bytes).ok()?;
        let id = old["subscription"]["id"].clone();
        let mut subscriptions = Vec::new();
        if let Some(subscription) = old["subscription"].as_object() {
            let mut subscription = subscription.clone();
            subscription.insert("client_id".into(), json!(self.config.subscriber_client_id));
            subscription.insert("event".into(), json!(EVENT_NAME));
            subscriptions.push(Value::Object(subscription));
        }
        let queue = old["queue"]
            .as_array()?
            .iter()
            .map(|pending| {
                let mut pending = pending.as_object()?.clone();
                let channel = pending.remove("channel_id")?;
                pending.insert("origin".into(), channel);
                pending.insert("subscription_id".into(), id.clone());
                Some(Value::Object(pending))
            })
            .collect::<Option<Vec<_>>>()?;
        serde_json::from_value(json!({
            "version": Self::STATE_VERSION,
            "subscriptions": subscriptions,
            "queue": queue,
            "seen": old["seen"],
            "threads": old["threads"],
            "source_revoked": old["source_revoked"],
        }))
        .ok()
    }

    fn fits(&self, state: &State<SlackState>) -> bool {
        state.source.seen.len() <= MAX_SEEN
            && state.source.threads.len() <= MAX_SEEN
            && state
                .queue
                .iter()
                .all(|p| p.origin.as_deref().is_some_and(valid_public_channel))
    }

    /// Static capability and catalog discovery lets the selected client
    /// request consent; subscribing and delivery need the scope as well.
    fn discoverable_to(&self, client_id: &str) -> bool {
        self.access.enabled() && client_id == self.config.subscriber_client_id
    }

    fn catalog(&self, _client_id: &str) -> Vec<Value> {
        vec![json!({"name": EVENT_NAME,
            "description": "Coworker mentions of the existing dot, and human follow-ups in participating threads across accessible public channels delivered by Slack. Owner and bot messages are excluded, apart from an explicitly enabled, expiring exact-marker owner proof.",
            "delivery": ["webhook"],
            "inputSchema": {"type":"object", "properties":{"scope":{"type":"string","enum":["accessible_public_channels"]}},"required":["scope"],"additionalProperties":false},
            "payloadSchema": {"type":"object", "properties": {
                "team_id":{"type":"string"},"channel_id":{"type":"string"},"user_id":{"type":"string"},
                "message_ts":{"type":"string"},"thread_ts":{"type":"string"},"text":{"type":"string"},
                "url":{"type":"string"},"reason":{"type":"string","enum":["mention","thread_reply"]}},
                "required":["team_id","channel_id","user_id","message_ts","thread_ts","text","url","reason"],"additionalProperties":false}})]
    }

    fn owns(&self, event: &str) -> bool {
        event == EVENT_NAME
    }

    fn resolve(&self, name: &Value, arguments: &Value) -> Result<String, EventError> {
        if name != EVENT_NAME || *arguments != scope_arguments() {
            return Err(invalid("unsupported event or scope"));
        }
        Ok(EVENT_NAME.to_owned())
    }

    fn subscription_id(
        &self,
        _client_id: &str,
        _event: &str,
        url: &str,
    ) -> Result<String, EventError> {
        // The configured source identity is part of the ID, so a changed
        // source config cannot reuse old grants.
        let key = serde_json::to_vec(&json!([self.config, EVENT_NAME, url, scope_arguments()]))
            .map_err(|_| unavailable())?;
        Ok(format!("sub_{}", hex::encode(Sha256::digest(key))))
    }

    fn accepts(&self, state: &SlackState) -> bool {
        !state.source_revoked
    }

    async fn permits(&self, _client_id: &str, _event: &str) -> Access {
        if !self.access.enabled() {
            return Access::Denied;
        }
        self.access.check(None).await
    }

    async fn admits(&self, channel: &str) -> Access {
        self.access.check(Some(channel)).await
    }

    /// Returns a URL-verification response or an empty event acknowledgement.
    /// A message is acknowledged only after its dedupe/queue state is durable.
    async fn ingest(
        hub: &Hub<Self>,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<Value, EventError> {
        let source = hub.source();
        let config = &source.config;
        if hub.is_degraded() || !source.access.enabled() {
            return Err(unavailable());
        }
        if body.len() > MAX_BODY {
            return Err(invalid("Slack event exceeds size limit"));
        }
        verify_slack(&source.source_secret, headers, body, now())?;
        let envelope: Value =
            serde_json::from_slice(body).map_err(|_| invalid("invalid Slack event JSON"))?;
        if envelope["type"] == "url_verification" {
            let challenge = envelope["challenge"]
                .as_str()
                .filter(|c| c.len() <= 1024)
                .ok_or_else(|| invalid("invalid Slack challenge"))?;
            return Ok(json!({"challenge":challenge}));
        }
        if envelope["api_app_id"] != config.app_id || envelope["team_id"] != config.team_id {
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
                    .is_some_and(|users| users.iter().any(|u| u == &config.owner_user_id)));
        // Stop revocations even when Slack omits authorizations on lifecycle events.
        // Wait for any in-flight callback before acknowledging the stop.
        let _revocation_gate = if revocation {
            Some(hub.hold_delivery().await)
        } else {
            None
        };
        // User-authorized source only, never borrow another app/bot installation.
        let authorized = envelope["authorizations"].as_array().is_some_and(|items| {
            items.iter().any(|a| {
                a["team_id"] == config.team_id
                    && a["user_id"] == config.owner_user_id
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
        let mut state = hub.state().await;
        let mut next = state.clone();
        next.source
            .seen
            .retain(|_, time| *time + REMEMBER_SECS > timestamp);
        next.source
            .threads
            .retain(|_, time| *time + REMEMBER_SECS > timestamp);
        if next.source.seen.contains_key(event_id) {
            return Ok(json!({}));
        }
        if !revocation && next.source.seen.len() >= MAX_SEEN {
            return Err(unavailable());
        }
        let subscription_id = next
            .live(timestamp)
            .next()
            .filter(|_| !next.source.source_revoked)
            .map(|s| s.id.clone());
        let public_message = event["type"] == "message"
            && event["channel"].as_str().is_some_and(valid_public_channel)
            && event["channel_type"] == "channel";
        if revocation {
            next.end_subscriptions(|_| true);
            next.queue.clear();
            next.source.seen.clear();
            next.source.threads.clear();
            next.source.source_revoked = true;
        } else if let Some(subscription_id) = subscription_id.filter(|_| public_message) {
            let channel = event["channel"].as_str().unwrap_or_default();
            if event["user"] == config.dot_user_id
                && event.get("subtype").is_none_or(|s| s == "bot_message")
            {
                // Observe the existing dot's participation, but never forward its
                // messages back into the dot. This also covers owner-started threads.
                if let Some(root) = event["thread_ts"]
                    .as_str()
                    .or_else(|| event["ts"].as_str())
                    .filter(|root| valid_slack_ts(root))
                {
                    let thread_key = format!("{channel}:{root}");
                    if next.source.threads.len() >= MAX_SEEN
                        && !next.source.threads.contains_key(&thread_key)
                    {
                        return Err(unavailable());
                    }
                    next.source.threads.insert(thread_key, timestamp);
                }
            } else if event.get("subtype").is_none()
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
                let owner_proof = user == config.owner_user_id
                    && config
                        .owner_proof_until
                        .is_some_and(|until| timestamp < until)
                    && text.trim() == format!("<@{}> {OWNER_PROOF_MARKER}", config.dot_user_id);
                if !user.is_empty()
                    && user != config.dot_user_id
                    && (user != config.owner_user_id || owner_proof)
                    && !ts.is_empty()
                    && valid_slack_ts(root)
                {
                    let mention = text.contains(&format!("<@{}>", config.dot_user_id));
                    let thread_key = format!("{channel}:{root}");
                    let reply = root != ts && next.source.threads.contains_key(&thread_key);
                    if mention || reply {
                        if next.queue.len() >= MAX_QUEUE
                            || (next.source.threads.len() >= MAX_SEEN
                                && !next.source.threads.contains_key(&thread_key))
                        {
                            return Err(unavailable());
                        }
                        let occurrence = ts
                            .split('.')
                            .next()
                            .and_then(|s| s.parse().ok())
                            .ok_or_else(|| invalid("invalid Slack timestamp"))?;
                        let body = serde_json::to_vec(&json!({"eventId":event_id,"name":EVENT_NAME,"timestamp":iso_time(occurrence)?,"cursor":null,
                            "data":{"team_id":config.team_id,"channel_id":channel,"user_id":user,
                                "message_ts":ts,"thread_ts":root,"text":text,"reason":if mention {"mention"} else {"thread_reply"},
                                "url":format!("https://app.slack.com/archives/{}/p{}",channel,ts.replace('.',""))}})).map_err(|_| unavailable())?;
                        if body.len() > MAX_BODY {
                            return Err(invalid("event payload exceeds size limit"));
                        }
                        if !owner_proof {
                            next.source.threads.insert(thread_key, timestamp);
                        }
                        next.queue.push(Pending::new(
                            subscription_id,
                            event_id.to_owned(),
                            Some(channel.to_owned()),
                            body,
                            timestamp,
                        ));
                    }
                }
            }
        }
        if !revocation
            && next.queue.len() == state.queue.len()
            && next.source.threads == state.source.threads
        {
            return Ok(json!({}));
        }
        next.source.seen.insert(event_id.to_owned(), timestamp);
        hub.commit(&mut state, next)?;
        hub.wake();
        Ok(json!({}))
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
    if timestamp.abs_diff(seconds) > SIGNATURE_WINDOW_SECS {
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
    if !bool::from(hmac_sha256(secret.as_str().as_bytes(), &signed).ct_eq(&supplied)) {
        return Err(denied());
    }
    Ok(())
}
