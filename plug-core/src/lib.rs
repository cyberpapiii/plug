#![forbid(unsafe_code)]
// RMCP 3.1 deprecates some APIs toward future SEP-2577. Plug intentionally
// retains them while MCP 2025-11-25 remains the default negotiated revision.
#![allow(deprecated)]

pub mod activity;
pub mod admission;
pub mod artifacts;
pub mod auth;
pub mod backoff;
pub mod branding;
pub mod circuit;
pub mod client_detect;
pub mod clients;
pub mod config;
pub mod dispatch;
pub mod doctor;
pub mod downstream_identity;
pub mod downstream_oauth;
pub mod engine;
pub mod enrichment;
pub mod error;
pub mod events;
pub mod fs_perm;
pub mod health;
pub mod http;
pub mod icons;
pub mod import;
pub mod ipc;
pub mod legacy_tasks;
pub(crate) mod mcp_http_headers;
pub mod notifications;
pub mod oauth;
pub mod openapi;
pub mod operator;
pub mod paths;
pub mod protocol;
pub mod proxy;
pub mod reload;
pub mod secrets;
pub mod server;
pub mod session;
pub mod slack_events;
pub(crate) mod supervision;
pub mod tasks;
pub mod tls;
pub mod tool_naming;
pub mod tool_patterns;
pub mod transport;
pub mod types;
pub mod upstream_http;
pub mod watcher;

// The Keychain on macOS. CI also runs on Linux, where the kernel keyring
// stands in.
#[cfg(not(target_os = "linux"))]
pub(crate) use keyring as platform_keyring;
#[cfg(target_os = "linux")]
pub(crate) use keyring_core as platform_keyring;

#[cfg(test)]
mod test_log;

#[cfg(any(test, feature = "test-helpers"))]
pub mod test_helpers;
