//! Fixtures shared by this crate's tests and by the tests of crates that
//! depend on it. Compiled only for tests and the `test-helpers` feature.

use crate::config::ServerConfig;

/// A stdio upstream running the harness's `mock-mcp-server` with `--tools`.
///
/// It runs the prebuilt binary instead of `cargo run`, so parallel tests do not
/// contend on Cargo's target lock.
pub fn mock_server(tools: &str) -> ServerConfig {
    ServerConfig {
        command: Some(
            plug_test_harness::mock_server_bin()
                .to_string_lossy()
                .into_owned(),
        ),
        args: vec!["--tools".to_string(), tools.to_string()],
        timeout_secs: 10,
        call_timeout_secs: 5,
        max_concurrent: 4,
        ..Default::default()
    }
}

/// SSE events from a response body, until the stream ends, `max_events`
/// arrive, or two seconds pass.
pub async fn collect_sse_events(body: axum::body::Body, max_events: usize) -> Vec<String> {
    use futures::StreamExt;

    let mut events = Vec::new();
    let mut stream = body.into_data_stream();

    let timeout = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while let Some(Ok(chunk)) = stream.next().await {
            let text = String::from_utf8_lossy(&chunk).to_string();
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
