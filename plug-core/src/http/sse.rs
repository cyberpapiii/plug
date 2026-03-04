use std::convert::Infallible;
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

/// Create an SSE stream from an mpsc receiver with disconnect detection.
///
/// - Sends a priming event as the first event (SHOULD per MCP spec 2025-11-25)
/// - Uses CancellationToken for graceful shutdown
/// - Uses `biased` select to prioritize shutdown over messages
/// - KeepAlive sends SSE comments (not events) to avoid confusing MCP clients
pub fn sse_stream(
    rx: mpsc::Receiver<serde_json::Value>,
    cancel: CancellationToken,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        // SSE priming event (SHOULD per spec 2025-11-25):
        // Empty data with event ID so clients know the stream is alive.
        yield Ok(Event::default().id("0").data(""));

        let mut rx = ReceiverStream::new(rx);
        use futures::StreamExt;

        let mut event_id: u64 = 1;
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
                            match serde_json::to_string(&msg) {
                                Ok(data) => {
                                    yield Ok(Event::default().id(event_id.to_string()).data(data));
                                    event_id += 1;
                                }
                                Err(e) => {
                                    tracing::error!(error = %e, "failed to serialize SSE message");
                                }
                            }
                        }
                        None => {
                            tracing::debug!("SSE sender dropped, closing stream");
                            break;
                        }
                    }
                }
            }
        }
    };

    Sse::new(stream).keep_alive(
        KeepAlive::new().interval(Duration::from_secs(15)).text(""), // SSE comment, not event — won't confuse MCP clients
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::response::IntoResponse;

    /// Collect SSE events from the response body until the stream ends or timeout.
    async fn collect_sse_events(body: Body, max_events: usize) -> Vec<String> {
        let mut events = Vec::new();
        let mut stream = body.into_data_stream();
        use futures::StreamExt;

        let timeout = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(Ok(chunk)) = stream.next().await {
                let text = String::from_utf8_lossy(&chunk).to_string();
                // Split into individual events (separated by double newlines)
                for part in text.split("\n\n") {
                    let trimmed = part.trim();
                    if !trimmed.is_empty() {
                        events.push(trimmed.to_string());
                    }
                }
                if events.len() >= max_events {
                    break;
                }
            }
        });

        let _ = timeout.await;
        events
    }

    #[tokio::test]
    async fn priming_event_is_first() {
        let (tx, rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();

        let sse = sse_stream(rx, cancel.clone());
        let response = sse.into_response();
        let body = response.into_body();

        // Drop sender so stream ends after priming event
        drop(tx);

        let events = collect_sse_events(body, 2).await;
        assert!(!events.is_empty(), "expected at least the priming event");
        // Priming event should have id: 0
        assert!(
            events[0].contains("id: 0"),
            "first event should have id 0, got: {}",
            events[0]
        );
    }

    #[tokio::test]
    async fn messages_forwarded_with_incrementing_ids() {
        let (tx, rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();

        let sse = sse_stream(rx, cancel.clone());
        let response = sse.into_response();
        let body = response.into_body();

        // Send two messages
        tx.send(serde_json::json!({"msg": "hello"})).await.unwrap();
        tx.send(serde_json::json!({"msg": "world"})).await.unwrap();
        drop(tx);

        let events = collect_sse_events(body, 4).await;
        // Should have: priming (id 0) + 2 messages (id 1, id 2)
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
        let (_tx, rx) = mpsc::channel::<serde_json::Value>(8);
        let cancel = CancellationToken::new();

        let sse = sse_stream(rx, cancel.clone());
        let response = sse.into_response();
        let body = response.into_body();

        // Cancel immediately after priming
        tokio::task::yield_now().await;
        cancel.cancel();

        let events = collect_sse_events(body, 5).await;
        // Should have at most the priming event — stream should close
        assert!(
            events.len() <= 2,
            "stream should close after cancel, got {} events",
            events.len()
        );
    }
}
