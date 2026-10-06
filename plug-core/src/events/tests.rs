use super::*;
use crate::slack_events::DeliveryResponse;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use reqwest::header::HeaderMap;
use std::sync::atomic::AtomicU64;

struct FakeAccess {
    watches: std::sync::Mutex<Vec<WatchConfig>>,
    result: std::sync::Mutex<Option<Value>>,
    read_only: AtomicBool,
    permitted: AtomicBool,
    calls: AtomicU64,
    /// A tool that never answers.
    stuck: std::sync::Mutex<Option<String>>,
}
#[async_trait::async_trait]
impl WatchAccess for FakeAccess {
    fn watches(&self) -> Vec<WatchConfig> {
        self.watches.lock().unwrap().clone()
    }
    fn may_use(&self, _: &str, _: &WatchConfig) -> bool {
        self.permitted.load(Ordering::SeqCst)
    }
    async fn permits(&self, _: &str, _: &WatchConfig) -> bool {
        self.permitted.load(Ordering::SeqCst)
    }
    fn tool(&self, watch: &WatchConfig) -> Option<(String, bool)> {
        Some((
            format!("{}__{}", watch.server, watch.tool),
            self.read_only.load(Ordering::SeqCst),
        ))
    }
    async fn call(&self, tool: &str, _: Map<String, Value>) -> Option<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.stuck.lock().unwrap().as_deref() == Some(tool) {
            std::future::pending::<()>().await;
        }
        self.result.lock().unwrap().clone()
    }
}

#[derive(Default)]
struct FakeDelivery {
    calls: Mutex<Vec<(HeaderMap, Vec<u8>)>>,
    status: AtomicU64,
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
        Ok(DeliveryResponse {
            status: self.status.load(Ordering::SeqCst) as u16,
            body: serde_json::to_vec(&json!({"challenge": value["challenge"]})).unwrap(),
        })
    }
}

fn watch() -> WatchConfig {
    WatchConfig {
        name: "inbox".into(),
        server: "mail".into(),
        tool: "unread".into(),
        arguments: Map::new(),
        every_secs: 60,
        allow_writes: false,
    }
}
fn params() -> Value {
    json!({"name":"mail.inbox","delivery":{"mode":"webhook","url":"https://events.example.com/callback","secret":format!("whsec_{}",STANDARD.encode([7u8;32]))}})
}
fn fixture() -> (
    tempfile::TempDir,
    Arc<WatchEvents>,
    Arc<FakeAccess>,
    Arc<FakeDelivery>,
) {
    let dir = tempfile::tempdir().unwrap();
    let access = Arc::new(FakeAccess {
        watches: std::sync::Mutex::new(vec![watch()]),
        result: std::sync::Mutex::new(Some(json!({"unread": 1}))),
        read_only: AtomicBool::new(true),
        permitted: AtomicBool::new(true),
        calls: AtomicU64::new(0),
        stuck: std::sync::Mutex::new(None),
    });
    let sender = Arc::new(FakeDelivery::default());
    sender.status.store(200, Ordering::SeqCst);
    let events =
        WatchEvents::open(&dir.path().join("watch"), access.clone(), sender.clone()).unwrap();
    (dir, events, access, sender)
}
/// Deliveries that are events, not the subscription challenge.
async fn delivered(sender: &FakeDelivery) -> Vec<Value> {
    sender
        .calls
        .lock()
        .await
        .iter()
        .map(|(_, body)| serde_json::from_slice::<Value>(body).unwrap())
        .filter(|body| body["type"] != "verification")
        .collect()
}

#[tokio::test]
async fn a_change_reaches_the_subscriber_and_the_first_result_does_not() {
    let (_dir, events, access, sender) = fixture();
    events.subscribe("client", &params()).await.unwrap();

    events.check(&watch()).await.unwrap();
    events.deliver_one().await.unwrap();
    assert!(delivered(&sender).await.is_empty(), "baseline is silent");

    events.check(&watch()).await.unwrap();
    events.deliver_one().await.unwrap();
    assert!(delivered(&sender).await.is_empty(), "same result is silent");

    *access.result.lock().unwrap() = Some(json!({"unread": 2}));
    events.check(&watch()).await.unwrap();
    events.deliver_one().await.unwrap();
    let sent = delivered(&sender).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["name"], "mail.inbox");
    assert_eq!(sent[0]["data"]["result"], json!({"unread": 2}));
    assert!(sent[0]["eventId"].as_str().unwrap().starts_with("evt_"));
    let calls = sender.calls.lock().await;
    assert!(calls.last().unwrap().0.contains_key("webhook-signature"));
}

#[tokio::test]
async fn a_tool_not_marked_read_only_is_not_called_unless_allowed() {
    let (_dir, events, access, _sender) = fixture();
    access.read_only.store(false, Ordering::SeqCst);
    events.check(&watch()).await.unwrap();
    assert_eq!(access.calls.load(Ordering::SeqCst), 0);

    let allowed = WatchConfig {
        allow_writes: true,
        ..watch()
    };
    events.check(&allowed).await.unwrap();
    assert_eq!(access.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_failed_call_is_not_a_change() {
    let (_dir, events, access, sender) = fixture();
    events.subscribe("client", &params()).await.unwrap();
    events.check(&watch()).await.unwrap();
    *access.result.lock().unwrap() = None;
    events.check(&watch()).await.unwrap();
    *access.result.lock().unwrap() = Some(json!({"unread": 1}));
    events.check(&watch()).await.unwrap();
    events.deliver_one().await.unwrap();
    assert!(delivered(&sender).await.is_empty());
}

#[tokio::test]
async fn a_client_kept_from_the_server_cannot_see_subscribe_or_receive() {
    let (_dir, events, access, sender) = fixture();
    events.subscribe("client", &params()).await.unwrap();
    events.check(&watch()).await.unwrap();
    *access.result.lock().unwrap() = Some(json!({"unread": 2}));
    events.check(&watch()).await.unwrap();

    access.permitted.store(false, Ordering::SeqCst);
    assert!(!events.discoverable_to("client"));
    assert!(events.catalog("client").is_empty());
    assert_eq!(
        events.subscribe("other", &params()).await.unwrap_err().code,
        -32001
    );
    events.deliver_one().await.unwrap();
    assert!(delivered(&sender).await.is_empty());
    assert!(events.state.lock().await.subscriptions.is_empty());
}

#[tokio::test]
async fn subscribe_rejects_unknown_events_arguments_and_replay() {
    let (_dir, events, _access, _sender) = fixture();
    let mut unknown = params();
    unknown["name"] = json!("mail.other");
    assert_eq!(
        events.subscribe("client", &unknown).await.unwrap_err().code,
        -32602
    );
    let mut arguments = params();
    arguments["arguments"] = json!({"folder": "x"});
    assert_eq!(
        events
            .subscribe("client", &arguments)
            .await
            .unwrap_err()
            .code,
        -32602
    );
    let mut replay = params();
    replay["cursor"] = json!("abc");
    assert_eq!(
        events.subscribe("client", &replay).await.unwrap_err().code,
        -32602
    );
    assert!(!events.owns("mail.other"));
    assert!(events.owns("mail.inbox"));
}

#[tokio::test]
async fn unsubscribe_and_a_removed_watch_both_stop_delivery() {
    let (_dir, events, access, sender) = fixture();
    events.subscribe("client", &params()).await.unwrap();
    events.unsubscribe("client", &params()).await.unwrap();
    events.check(&watch()).await.unwrap();
    *access.result.lock().unwrap() = Some(json!({"unread": 2}));
    events.check(&watch()).await.unwrap();
    events.deliver_one().await.unwrap();
    assert!(delivered(&sender).await.is_empty());

    events.subscribe("client", &params()).await.unwrap();
    access.watches.lock().unwrap().clear();
    events.prune().await.unwrap();
    let state = events.state.lock().await;
    assert!(state.subscriptions.is_empty() && state.baselines.is_empty());
}

#[tokio::test]
async fn a_gone_callback_removes_the_subscription_and_state_survives_reopen() {
    let (dir, events, access, sender) = fixture();
    events.subscribe("client", &params()).await.unwrap();
    events.check(&watch()).await.unwrap();
    drop(events);
    let events =
        WatchEvents::open(&dir.path().join("watch"), access.clone(), sender.clone()).unwrap();
    assert_eq!(events.state.lock().await.subscriptions.len(), 1);

    *access.result.lock().unwrap() = Some(json!({"unread": 2}));
    events.check(&watch()).await.unwrap();
    sender.status.store(410, Ordering::SeqCst);
    events.deliver_one().await.unwrap();
    let state = events.state.lock().await;
    assert!(state.subscriptions.is_empty() && state.queue.is_empty());
}

#[test]
fn config_rejects_bad_names_short_intervals_and_duplicates() {
    let bad = EventsConfig {
        watch: vec![
            WatchConfig {
                name: "Bad Name".into(),
                every_secs: 5,
                ..watch()
            },
            watch(),
            watch(),
        ],
    };
    let errors = bad.validate();
    assert_eq!(errors.len(), 3, "{errors:?}");
    assert!(
        EventsConfig {
            watch: vec![watch()]
        }
        .validate()
        .is_empty()
    );
}

#[tokio::test]
async fn a_check_waiting_its_turn_does_not_run_once_the_watch_is_gone_or_plug_is_stopping() {
    let (_dir, events, access, _sender) = fixture();
    let settle = || tokio::time::sleep(Duration::from_millis(100));
    let busy = events
        .check_slots
        .clone()
        .acquire_many_owned(CHECKS_AT_ONCE as u32)
        .await
        .unwrap();

    // Removed while it waited.
    events.check_due(&[watch()], &mut HashMap::new(), &CancellationToken::new());
    settle().await;
    access.watches.lock().unwrap().clear();
    drop(busy);
    settle().await;
    assert_eq!(access.calls.load(Ordering::SeqCst), 0);
    assert!(events.checking.lock().unwrap().is_empty());

    // Still configured, but Plug is stopping.
    *access.watches.lock().unwrap() = vec![watch()];
    let busy = events
        .check_slots
        .clone()
        .acquire_many_owned(CHECKS_AT_ONCE as u32)
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    events.check_due(&[watch()], &mut HashMap::new(), &cancel);
    settle().await;
    cancel.cancel();
    settle().await;
    drop(busy);
    settle().await;
    assert_eq!(access.calls.load(Ordering::SeqCst), 0);
    assert!(events.checking.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_tool_that_does_not_answer_does_not_hold_up_the_other_watches() {
    let (_dir, events, access, _sender) = fixture();
    let slow = WatchConfig {
        name: "slow".into(),
        tool: "search".into(),
        ..watch()
    };
    *access.stuck.lock().unwrap() = Some("mail__search".into());
    let watches = vec![slow, watch()];
    *access.watches.lock().unwrap() = watches.clone();
    let mut due = HashMap::new();
    let settle = || tokio::time::sleep(Duration::from_millis(100));

    // Starting a round never waits on a tool.
    events.check_due(&watches, &mut due, &CancellationToken::new());
    settle().await;
    assert_eq!(access.calls.load(Ordering::SeqCst), 2);
    assert!(
        events.checked.lock().unwrap()["mail.inbox"]
            .last_checked
            .is_some()
    );
    assert!(!events.checked.lock().unwrap().contains_key("mail.slow"));

    // Both come due again while the slow tool still has not answered. The
    // other watch gets its next check on time, and the stuck tool is not
    // called a second time on top of the first.
    due.clear();
    events.check_due(&watches, &mut due, &CancellationToken::new());
    settle().await;
    assert_eq!(access.calls.load(Ordering::SeqCst), 3);
    assert!(!due.contains_key("mail.slow"), "still on its first check");
}
