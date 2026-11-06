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
