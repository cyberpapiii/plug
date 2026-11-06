use std::convert::Infallible;
use std::time::Duration;

use axum::response::sse::{Event, Sse};
use futures::stream::{Stream, StreamExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::session::SseEvent;

/// SSE stream from an mpsc receiver with priming event, keepalive comments
/// (each one calling `on_keepalive`), and cancel.
pub fn sse_stream_with_heartbeat<F>(
    rx: mpsc::Receiver<SseEvent>,
    cancel: CancellationToken,
    on_keepalive: F,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    F: FnMut() + Send + 'static,
{
    sse_stream_with_heartbeat_interval(rx, cancel, Duration::from_secs(15), on_keepalive)
}

fn sse_stream_with_heartbeat_interval<F>(
    rx: mpsc::Receiver<SseEvent>,
    cancel: CancellationToken,
    keepalive_interval: Duration,
    mut on_keepalive: F,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
where
    F: FnMut() + Send + 'static,
{
    let stream = async_stream::stream! {
        yield Ok(Event::default().id("0").data(""));

        let mut rx = ReceiverStream::new(rx);

        let mut keepalive = tokio::time::interval(keepalive_interval);
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        keepalive.tick().await;
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    tracing::debug!("SSE stream cancelled via shutdown token");
                    break;
                }
                msg = rx.next() => {
                    match msg {
                        Some(msg) => {
                            yield Ok(Event::default().id(msg.id.to_string()).data(msg.message.as_str()));
                        }
                        None => {
                            tracing::debug!("SSE sender dropped, closing stream");
                            break;
                        }
                    }
                }
                _ = keepalive.tick() => {
                    on_keepalive();
                    yield Ok(Event::default().comment(""));
                }
            }
        }
    };

    Sse::new(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::collect_sse_events;
    use axum::response::IntoResponse;

    #[tokio::test]
    async fn priming_event_is_first() {
        let (tx, rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();

        let sse = sse_stream_with_heartbeat(rx, cancel.clone(), || {});
        let response = sse.into_response();
        let body = response.into_body();

        drop(tx);

        let events = collect_sse_events(body, 2).await;
        assert!(!events.is_empty(), "expected at least the priming event");
        assert!(
            events[0].contains("id: 0"),
            "first event should have id 0, got: {}",
            events[0]
        );
    }

    #[tokio::test]
    async fn messages_are_forwarded_with_their_ids() {
        let (tx, rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();

        let sse = sse_stream_with_heartbeat(rx, cancel.clone(), || {});
        let response = sse.into_response();
        let body = response.into_body();

        tx.send(SseEvent {
            id: 1,
            message: crate::session::SseMessage::from_json_value(
                serde_json::json!({"msg": "hello"}),
            )
            .unwrap(),
        })
        .await
        .unwrap();
        tx.send(SseEvent {
            id: 2,
            message: crate::session::SseMessage::from_json_value(
                serde_json::json!({"msg": "world"}),
            )
            .unwrap(),
        })
        .await
        .unwrap();
        drop(tx);

        let events = collect_sse_events(body, 4).await;
        assert!(events.len() >= 3, "expected 3 events, got: {events:?}");
        assert!(events[1].contains("id: 1"), "second event should be id 1");
        assert!(events[2].contains("id: 2"), "third event should be id 2");
        assert!(
            events[1].contains("hello"),
            "event 1 should contain message"
        );
    }

    #[tokio::test]
    async fn cancellation_closes_stream() {
        let (_tx, rx) = mpsc::channel::<SseEvent>(8);
        let cancel = CancellationToken::new();

        let sse = sse_stream_with_heartbeat(rx, cancel.clone(), || {});
        let mut body = sse.into_response().into_body().into_data_stream();
        cancel.cancel();

        // The sender is still held and the keepalive is 15 s away, so only
        // the cancel can end the stream within the timeout.
        let mut chunks = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(chunk) = body.next().await {
                chunks.push(chunk.expect("SSE chunk"));
            }
        })
        .await
        .expect("the stream ends once cancelled");
        assert_eq!(chunks.len(), 1, "only the priming event: {chunks:?}");
    }

    #[tokio::test]
    async fn heartbeat_emits_comment_and_runs_callback() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (_tx, rx) = mpsc::channel::<SseEvent>(8);
        let cancel = CancellationToken::new();
        let heartbeats = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = std::sync::Arc::clone(&heartbeats);

        let sse = sse_stream_with_heartbeat_interval(
            rx,
            cancel.clone(),
            Duration::from_millis(10),
            move || {
                seen.fetch_add(1, Ordering::SeqCst);
            },
        );
        let response = sse.into_response();
        let body = response.into_body();

        let events = collect_sse_events(body, 2).await;
        cancel.cancel();

        assert!(heartbeats.load(Ordering::SeqCst) >= 1);
        assert!(
            events.iter().any(|event| event.starts_with(":")),
            "expected at least one keepalive comment event, got: {events:?}"
        );
    }
}
