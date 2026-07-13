#![forbid(unsafe_code)]
// RMCP 2.2 deprecates these APIs toward future SEP-2577. Plug intentionally
// retains them while MCP 2025-11-25 remains the negotiated stable revision.
#![allow(deprecated)]

pub mod artifacts;
pub mod auth;
pub mod branding;
pub mod circuit;
pub mod client_detect;
pub mod config;
pub mod dispatch;
pub mod doctor;
pub mod dotenv;
pub mod downstream_oauth;
pub mod engine;
pub mod enrichment;
pub mod error;
pub mod export;
pub mod fs_perm;
pub mod health;
pub mod http;
pub mod icons;
pub mod import;
pub mod ipc;
pub(crate) mod mcp_http_headers;
pub mod notifications;
pub mod oauth;
pub mod protocol;
pub mod proxy;
pub mod reload;
pub mod server;
pub mod session;
pub mod tasks;
pub mod tls;
pub mod tool_naming;
pub mod transport;
pub mod types;
pub mod watcher;
