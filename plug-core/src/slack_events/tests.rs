use super::*;
use crate::events::{DeliveryResponse, EventSource};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tokio::sync::Mutex;

/// Answers every access check with what it holds.
struct FakeAccess(std::sync::Mutex<Access>);
impl FakeAccess {
    fn allowed() -> Self {
        Self(std::sync::Mutex::new(Access::Allowed))
    }
    fn set(&self, access: Access) {
        *self.0.lock().unwrap() = access;
    }
}
#[async_trait::async_trait]
impl EventAccess for FakeAccess {
    fn enabled(&self) -> bool {
        true
    }
    async fn check(&self, _channel: Option<&str>) -> Access {
        *self.0.lock().unwrap()
    }
}
#[derive(Default)]
struct FakeDelivery {
    calls: Mutex<Vec<(HeaderMap, Vec<u8>)>>,
    status: AtomicU64,
    bad_echo: AtomicBool,
}
#[async_trait::async_trait]
impl EventDelivery for FakeDelivery {
    async fn post(
        &self,
        _: &str,
        headers: HeaderMap,
        body: Vec<u8>,
    ) -> Result<DeliveryResponse, &'static str> {
        let value: Value = serde_json::from_slice(&body).unwrap();
        self.calls.lock().await.push((headers, body));
        let response = if value["type"] == "verification" {
            if self.bad_echo.load(Ordering::SeqCst) {
                json!({"challenge":"wrong"})
            } else {
                json!({"challenge":value["challenge"]})
            }
        } else {
            json!({})
        };
        Ok(DeliveryResponse {
            status: self.status.load(Ordering::SeqCst) as u16,
            body: serde_json::to_vec(&response).unwrap(),
        })
    }
}
fn config() -> SlackEventsConfig {
    SlackEventsConfig {
        team_id: "T123".into(),
        app_id: "A123".into(),
        owner_user_id: "U123".into(),
        dot_user_id: "U456".into(),
        subscriber_client_id: "existing-client".into(),
        owner_proof_until: None,
    }
}
fn params() -> Value {
    json!({"name":EVENT_NAME,"arguments":{"scope":"accessible_public_channels"},"delivery":{"mode":"webhook","url":"https://events.example.com/callback","secret":format!("whsec_{}",STANDARD.encode([7u8;32]))},"cursor":null})
}
fn fixture() -> (
    tempfile::TempDir,
    Arc<SlackEvents>,
    Arc<FakeAccess>,
    Arc<FakeDelivery>,
) {
    let dir = tempfile::tempdir().unwrap();
    let access = Arc::new(FakeAccess::allowed());
    let sender = Arc::new(FakeDelivery::default());
    sender.status.store(200, Ordering::SeqCst);
    let events = SlackEvents::open(
        config(),
        "fake-signing-secret".to_owned().into(),
        &dir.path().join("events"),
        access.clone(),
        sender.clone(),
    )
    .unwrap();
    (dir, events, access, sender)
}
fn envelope(id: &str, text: &str, thread: Option<&str>) -> Value {
    let mut value = json!({"type":"event_callback","api_app_id":"A123","team_id":"T123","event_id":id,"authorizations":[{"team_id":"T123","user_id":"U123","is_bot":false}],"event":{"type":"message","channel":"C123","channel_type":"channel","user":"U789","ts":"1700000000.123456","text":text}});
    if let Some(thread) = thread {
        value["event"]["thread_ts"] = json!(thread);
        value["event"]["ts"] = json!("1700000001.123456");
    }
    value
}
fn slack_headers(body: &[u8], timestamp: u64) -> HeaderMap {
    let mut mac = Hmac::<Sha256>::new_from_slice(b"fake-signing-secret").unwrap();
    mac.update(format!("v0:{timestamp}:").as_bytes());
    mac.update(body);
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-slack-request-timestamp",
        timestamp.to_string().parse().unwrap(),
    );
    headers.insert(
        "x-slack-signature",
        format!("v0={}", hex::encode(mac.finalize().into_bytes()))
            .parse()
            .unwrap(),
    );
    headers
}
async fn ingest(events: &SlackEvents, value: Value) -> Result<Value, EventError> {
    let body = serde_json::to_vec(&value).unwrap();
    events.ingest(&slack_headers(&body, now()), &body).await
}

#[tokio::test]
async fn mention_reply_dedupe_restart_and_unsubscribe() {
    let (dir, events, access, sender) = fixture();
    let subscribed = events
        .subscribe("existing-client", &params())
        .await
        .unwrap();
    assert!(subscribed["refreshBefore"].is_string());
    ingest(&events, envelope("Ev1", "<@U456> harmless canary", None))
        .await
        .unwrap();
    ingest(&events, envelope("Ev1", "<@U456> duplicate", None))
        .await
        .unwrap();
    ingest(
        &events,
        envelope("Ev2", "unmentioned reply", Some("1700000000.123456")),
    )
    .await
    .unwrap();
    assert_eq!(events.state().await.queue.len(), 2);
    drop(events);
    let events = SlackEvents::open(
        config(),
        "fake-signing-secret".to_owned().into(),
        &dir.path().join("events"),
        access,
        sender.clone(),
    )
    .unwrap();
    events.deliver_one().await.unwrap();
    events.deliver_one().await.unwrap();
    assert!(events.state().await.queue.is_empty());
    let calls = sender.calls.lock().await;
    assert_eq!(calls.len(), 3);
    let first: Value = serde_json::from_slice(&calls[1].1).unwrap();
    let reply: Value = serde_json::from_slice(&calls[2].1).unwrap();
    assert_eq!(first["eventId"], "Ev1");
    assert_eq!(reply["data"]["reason"], "thread_reply");
    assert!(calls[1].0.contains_key("webhook-signature"));
    drop(calls);
    events
        .unsubscribe("existing-client", &params())
        .await
        .unwrap();
    events
        .unsubscribe("existing-client", &params())
        .await
        .unwrap();
    ingest(&events, envelope("Ev3", "<@U456> after removal", None))
        .await
        .unwrap();
    assert!(events.state().await.queue.is_empty());
}
#[tokio::test]
async fn rejects_bad_signature_replay_workspace_and_installation() {
    let (_dir, events, _, _) = fixture();
    events
        .subscribe("existing-client", &params())
        .await
        .unwrap();
    let body = serde_json::to_vec(&envelope("Ev1", "<@U456>", None)).unwrap();
    assert!(events.ingest(&HeaderMap::new(), &body).await.is_err());
    assert!(
        events
            .ingest(&slack_headers(&body, now() - 301), &body)
            .await
            .is_err()
    );
    let mut tampered = body.clone();
    tampered.push(b' ');
    assert!(
        events
            .ingest(&slack_headers(&body, now()), &tampered)
            .await
            .is_err()
    );
    for field in ["team_id", "api_app_id"] {
        let mut value = envelope("Ev1", "<@U456>", None);
        value[field] = json!("WRONG");
        assert!(ingest(&events, value).await.is_err());
    }
    let mut value = envelope("Ev1", "<@U456>", None);
    value["authorizations"][0]["is_bot"] = json!(true);
    assert!(ingest(&events, value).await.is_err());
    assert!(events.state().await.queue.is_empty());
}
#[tokio::test]
async fn ignores_invalid_channels_dms_bots_owner_and_untracked_threads() {
    let (_dir, events, _, _) = fixture();
    events
        .subscribe("existing-client", &params())
        .await
        .unwrap();
    for (i, (field, value)) in [
        ("channel", json!("D999")),
        ("channel_type", json!("im")),
        ("bot_id", json!("B123")),
        ("subtype", json!("message_changed")),
        ("user", json!("U123")),
        ("user", json!("U456")),
    ]
    .into_iter()
    .enumerate()
    {
        let mut e = envelope(&format!("Ev{i}"), "<@U456>", None);
        e["event"][field] = value;
        ingest(&events, e).await.unwrap();
    }
    ingest(
        &events,
        envelope("Ev7", "ordinary reply", Some("1700000009.123456")),
    )
    .await
    .unwrap();
    assert!(events.state().await.queue.is_empty());
}
#[tokio::test]
async fn retries_keep_event_id_and_body_and_revoked_access_stops() {
    let (_dir, events, access, sender) = fixture();
    events
        .subscribe("existing-client", &params())
        .await
        .unwrap();
    ingest(&events, envelope("Ev1", "<@U456>", None))
        .await
        .unwrap();
    sender.status.store(503, Ordering::SeqCst);
    events.deliver_one().await.unwrap();
    assert_eq!(events.state().await.queue[0].attempts, 1);
    events.state().await.queue[0].next_at = 0;
    sender.status.store(200, Ordering::SeqCst);
    events.deliver_one().await.unwrap();
    let calls = sender.calls.lock().await;
    assert_eq!(calls[1].1, calls[2].1);
    assert_eq!(calls[1].0["webhook-id"], calls[2].0["webhook-id"]);
    drop(calls);
    ingest(&events, envelope("Ev2", "<@U456>", None))
        .await
        .unwrap();
    access.set(Access::Denied);
    events.deliver_one().await.unwrap();
    assert!(events.state().await.subscriptions.is_empty());
    assert_eq!(sender.calls.lock().await.len(), 3);
}
#[tokio::test]
async fn verification_failure_and_invalid_parameters_create_no_subscription() {
    let (_dir, events, _, sender) = fixture();
    sender.bad_echo.store(true, Ordering::SeqCst);
    assert_eq!(
        events
            .subscribe("existing-client", &params())
            .await
            .unwrap_err()
            .code,
        -32015
    );
    assert!(events.state().await.subscriptions.is_empty());
    for (key, value) in [
        ("cursor", json!("unsupported")),
        ("ttlMs", json!(0)),
        ("arguments", json!({"channel_id":"C999"})),
    ] {
        let mut p = params();
        p[key] = value;
        assert!(events.subscribe("existing-client", &p).await.is_err());
    }
    let mut p = params();
    p["delivery"]["secret"] = json!("whsec_bad");
    assert!(events.subscribe("existing-client", &p).await.is_err());
}
#[tokio::test]
async fn expiry_permanent_failure_rotation_and_single_writer() {
    let (dir, events, access, sender) = fixture();
    let first = events
        .subscribe("existing-client", &params())
        .await
        .unwrap();
    assert!(
        SlackEvents::open(
            config(),
            "fake-signing-secret".to_owned().into(),
            &dir.path().join("events"),
            access,
            sender.clone()
        )
        .is_err()
    );
    let mut rotated = params();
    rotated["delivery"]["secret"] = json!(format!("whsec_{}", STANDARD.encode([8u8; 32])));
    let next = events.subscribe("existing-client", &rotated).await.unwrap();
    assert_eq!(first["id"], next["id"]);
    events.subscribe("existing-client", &rotated).await.unwrap();
    ingest(&events, envelope("Ev1", "<@U456>", None))
        .await
        .unwrap();
    sender.status.store(413, Ordering::SeqCst);
    events.deliver_one().await.unwrap();
    assert!(events.state().await.queue.is_empty());
    let calls = sender.calls.lock().await;
    assert_eq!(
        calls.last().unwrap().0["webhook-signature"]
            .to_str()
            .unwrap()
            .split(' ')
            .count(),
        2
    );
    drop(calls);
    events.state().await.subscriptions[0].expires = 0;
    events.deliver_one().await.unwrap();
    assert!(events.state().await.subscriptions.is_empty());
}

#[tokio::test]
async fn signed_lifecycle_revocation_stops_delivery() {
    let (_dir, events, _, sender) = fixture();
    events
        .subscribe("existing-client", &params())
        .await
        .unwrap();
    ingest(&events, envelope("Ev1", "<@U456>", None))
        .await
        .unwrap();
    events.state().await.source.seen = (0..MAX_SEEN).map(|i| (format!("Seen{i}"), now())).collect();
    let mut revoke = envelope("Ev2", "", None);
    revoke["event"] = json!({"type":"tokens_revoked","tokens":{"oauth":["U123"]}});
    revoke.as_object_mut().unwrap().remove("authorizations");
    ingest(&events, revoke).await.unwrap();
    events.deliver_one().await.unwrap();
    assert_eq!(sender.calls.lock().await.len(), 1);
    assert!(
        events
            .subscribe("existing-client", &params())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_version_2_state_file_keeps_its_subscription_and_queue() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("events");
    crate::fs_perm::ensure_dir_0700(&state_dir).unwrap();
    let secret = format!("whsec_{}", STANDARD.encode([7u8; 32]));
    let old = json!({
        "version": 2,
        "subscription": {"id": "sub_old", "url": "https://events.example.com/callback",
            "secret": secret, "old_secret": null, "old_secret_until": 0,
            "expires": now() + 3600, "verified_until": 0},
        "seen": {"Ev0": now()},
        "threads": {"C123:1700000000.123456": now()},
        "queue": [{"event_id": "Ev1", "channel_id": "C123", "body": b"{}".to_vec(),
            "attempts": 1, "next_at": 0}],
        "source_revoked": false,
    });
    let path = state_dir.join("state.json");
    std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let sender = Arc::new(FakeDelivery::default());
    sender.status.store(200, Ordering::SeqCst);
    let events = SlackEvents::open(
        config(),
        "fake-signing-secret".to_owned().into(),
        &state_dir,
        Arc::new(FakeAccess::allowed()),
        sender.clone(),
    )
    .unwrap();
    {
        let state = events.state().await;
        assert_eq!(state.subscriptions[0].client_id, "existing-client");
        assert_eq!(state.queue[0].origin.as_deref(), Some("C123"));
        assert!(state.source.seen.contains_key("Ev0"));
        assert_eq!(state.source.threads.len(), 1);
    }
    events.deliver_one().await.unwrap();
    let calls = sender.calls.lock().await;
    assert_eq!(calls[0].0["x-mcp-subscription-id"], "sub_old");
    assert_eq!(calls[0].0["webhook-id"], "Ev1");
}

#[tokio::test]
async fn existing_dot_participation_tracks_owner_started_thread_without_feedback() {
    let (_dir, events, _, _) = fixture();
    events
        .subscribe("existing-client", &params())
        .await
        .unwrap();
    let mut owner = envelope("EvOwner", "<@U456> owner starts this", None);
    owner["event"]["user"] = json!("U123");
    ingest(&events, owner).await.unwrap();
    let mut dot = envelope("EvDot", "native dot reply", Some("1700000000.123456"));
    dot["event"]["user"] = json!("U456");
    dot["event"]["bot_id"] = json!("B123");
    dot["event"]["subtype"] = json!("bot_message");
    dot["event"]["app_id"] = json!("A_NATIVE_DOT");
    ingest(&events, dot).await.unwrap();
    assert!(events.state().await.queue.is_empty());
    ingest(
        &events,
        envelope(
            "EvCoworker",
            "coworker follows up without mention",
            Some("1700000000.123456"),
        ),
    )
    .await
    .unwrap();
    let state = events.state().await;
    assert_eq!(state.queue.len(), 1);
    let value: Value = serde_json::from_slice(&state.queue[0].body).unwrap();
    assert_eq!(value["eventId"], "EvCoworker");
    assert_eq!(value["data"]["reason"], "thread_reply");
}

#[tokio::test]
async fn public_channels_are_dynamic_and_threads_are_channel_specific() {
    let (dir, events, access, sender) = fixture();
    let subscription = events
        .subscribe("existing-client", &params())
        .await
        .unwrap();
    ingest(&events, envelope("EvA", "<@U456>", None))
        .await
        .unwrap();
    let mut unrelated = envelope("EvB", "unmentioned", Some("1700000000.123456"));
    unrelated["event"]["channel"] = json!("C999");
    ingest(&events, unrelated).await.unwrap();
    assert_eq!(events.state().await.queue.len(), 1);
    drop(events);
    let events = SlackEvents::open(
        config(),
        "fake-signing-secret".to_owned().into(),
        &dir.path().join("events"),
        access,
        sender,
    )
    .unwrap();
    let mut new_channel = envelope("EvC", "<@U456>", None);
    new_channel["event"]["channel"] = json!("CNEW");
    ingest(&events, new_channel.clone()).await.unwrap();
    ingest(&events, new_channel).await.unwrap();
    let mut reply = envelope("EvD", "unmentioned", Some("1700000000.123456"));
    reply["event"]["channel"] = json!("CNEW");
    ingest(&events, reply).await.unwrap();
    let state = events.state().await;
    assert_eq!(state.queue.len(), 3);
    assert_eq!(
        state.subscriptions[0].id,
        subscription["id"].as_str().unwrap()
    );
    let body: Value = serde_json::from_slice(&state.queue[2].body).unwrap();
    assert_eq!(body["data"]["channel_id"], "CNEW");
    assert_eq!(body["data"]["reason"], "thread_reply");
}

struct ChannelAccess(Access);
#[async_trait::async_trait]
impl EventAccess for ChannelAccess {
    fn enabled(&self) -> bool {
        true
    }
    async fn check(&self, channel: Option<&str>) -> Access {
        if channel == Some("C123") {
            self.0
        } else {
            Access::Allowed
        }
    }
}
#[tokio::test]
async fn inaccessible_channel_does_not_block_other_channels_or_revoke_subscription() {
    for decision in [Access::Denied, Access::Unavailable] {
        let dir = tempfile::tempdir().unwrap();
        let sender = Arc::new(FakeDelivery::default());
        sender.status.store(200, Ordering::SeqCst);
        let events = SlackEvents::open(
            config(),
            "fake-signing-secret".to_owned().into(),
            &dir.path().join("events"),
            Arc::new(ChannelAccess(decision)),
            sender.clone(),
        )
        .unwrap();
        events
            .subscribe("existing-client", &params())
            .await
            .unwrap();
        ingest(&events, envelope("EvA", "<@U456>", None))
            .await
            .unwrap();
        let mut second = envelope("EvB", "<@U456>", None);
        second["event"]["channel"] = json!("C999");
        ingest(&events, second).await.unwrap();
        events.deliver_one().await.unwrap();
        events.deliver_one().await.unwrap();
        let calls = sender.calls.lock().await;
        assert_eq!(calls.len(), 2); // verification and accessible C999 only
        let body: Value = serde_json::from_slice(&calls[1].1).unwrap();
        assert_eq!(body["data"]["channel_id"], "C999");
        let state = events.state().await;
        assert!(!state.subscriptions.is_empty());
        if decision == Access::Denied {
            assert!(!state.source.threads.contains_key("C123:1700000000.123456"));
        }
    }
}
#[tokio::test]
async fn subscription_requires_explicit_public_scope_and_rejects_channel_selectors() {
    let (_dir, events, _, _) = fixture();
    for arguments in [
        json!({}),
        json!({"scope":"*"}),
        json!({"scope":"all_channels"}),
        json!({"scope":"accessible_public_channels","channel_id":"C123"}),
        json!({"channel_id":"C123"}),
        json!({"scope":"private_channels"}),
    ] {
        let mut request = params();
        request["arguments"] = arguments;
        assert!(events.subscribe("existing-client", &request).await.is_err());
    }
    let mut old_config = serde_json::to_value(config()).unwrap();
    old_config["channel_id"] = json!("C123");
    assert!(serde_json::from_value::<SlackEventsConfig>(old_config).is_err());
    assert_eq!(
        events.catalog("")[0]["inputSchema"]["properties"]["scope"]["enum"][0],
        "accessible_public_channels"
    );
}

#[tokio::test]
async fn owner_proof_only_accepts_exact_public_human_marker_before_expiry() {
    for until in [None, Some(now() - 1), Some(now() + 3600)] {
        let (dir, _original, access, sender) = fixture();
        let mut value = serde_json::to_value(config()).unwrap();
        if let Some(until) = until {
            value["owner_proof_until"] = json!(until);
        }
        let cfg: SlackEventsConfig = serde_json::from_value(value).unwrap();
        let events = SlackEvents::open(
            cfg,
            "fake-signing-secret".to_owned().into(),
            &dir.path().join("proof-events"),
            access,
            sender,
        )
        .unwrap();
        events
            .subscribe("existing-client", &params())
            .await
            .unwrap();
        let marker = "<@U456> PLUG_EVENTS_PROOF_20261003";
        for (i, text) in [
            marker,
            "<@U456> ordinary owner mention",
            "<@U456> PLUG_EVENTS_PROOF_20261003 extra",
            "PLUG_EVENTS_PROOF_20261003",
        ]
        .iter()
        .enumerate()
        {
            let mut event = envelope(&format!("EvOwnerProof{i}"), text, None);
            event["event"]["user"] = json!("U123");
            ingest(&events, event).await.unwrap();
        }
        for (i, (field, value)) in [("channel_type", json!("im")), ("bot_id", json!("B123"))]
            .into_iter()
            .enumerate()
        {
            let mut event = envelope(&format!("EvOwnerIgnored{i}"), marker, None);
            event["event"]["user"] = json!("U123");
            event["event"][field] = value;
            ingest(&events, event).await.unwrap();
        }
        let expected = usize::from(until.is_some_and(|expiry| expiry > now()));
        assert_eq!(events.state().await.queue.len(), expected);
        assert!(events.state().await.source.threads.is_empty());
    }
}

#[test]
fn owner_proof_window_preserves_existing_subscription_storage_key() {
    let baseline = config();
    let mut proof = baseline.clone();
    proof.owner_proof_until = Some(now() + 3600);
    assert_eq!(state_dir(&baseline).unwrap(), state_dir(&proof).unwrap());
    proof.owner_proof_until = Some(now() + 3601);
    assert!(!proof.validate().is_empty());
}

#[tokio::test]
async fn an_owner_proof_window_refreshes_and_unsubscribes_the_existing_subscription() {
    let (dir, original, access, sender) = fixture();
    let first = original
        .subscribe("existing-client", &params())
        .await
        .unwrap();
    drop(original);
    let mut proof = config();
    proof.owner_proof_until = Some(now() + 3600);
    let events = SlackEvents::open(
        proof,
        "fake-signing-secret".to_owned().into(),
        &dir.path().join("events"),
        access,
        sender,
    )
    .unwrap();
    let refreshed = events
        .subscribe("existing-client", &params())
        .await
        .unwrap();
    assert_eq!(first["id"], refreshed["id"]);
    events
        .unsubscribe("existing-client", &params())
        .await
        .unwrap();
    assert!(events.state().await.subscriptions.is_empty());
}

#[tokio::test]
async fn only_the_chosen_client_may_subscribe() {
    let (_dir, events, _, _) = fixture();
    let error = events
        .subscribe("another-client", &params())
        .await
        .unwrap_err();
    assert_eq!(error.code, denied().code);
    assert!(events.state().await.subscriptions.is_empty());
}

#[tokio::test]
async fn unknown_access_waits_out_a_backoff_before_asking_again() {
    let (_dir, events, access, sender) = fixture();
    events
        .subscribe("existing-client", &params())
        .await
        .unwrap();
    ingest(&events, envelope("Ev1", "<@U456>", None))
        .await
        .unwrap();
    access.set(Access::Unavailable);
    events.deliver_one().await.unwrap();
    let pending = events.state().await.queue[0].clone();
    assert_eq!(pending.attempts, 1);
    assert!(pending.next_at > now());
    assert_eq!(sender.calls.lock().await.len(), 1, "only the challenge");
}
