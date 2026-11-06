//! The stdio side of `plug connect`: rewrites between the host's protocol
//! era and the one the proxy handler speaks, line by line.

use std::sync::Arc;

fn is_modern_stdio_message(value: &serde_json::Value) -> bool {
    value.get("method").and_then(serde_json::Value::as_str) == Some("server/discover")
        || value
            .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
            .and_then(serde_json::Value::as_str)
            == Some(plug_core::protocol::MODERN_PROTOCOL_VERSION)
}

#[derive(Default)]
struct StdioProtocolState {
    modern_confirmed: bool,
    pending_discovery_ids: std::collections::HashSet<String>,
    /// Plain `initialize` requests that asked for `2026-07-28`. The era is
    /// confirmed only when the reply negotiates that same version.
    pending_modern_initialize_ids: std::collections::HashSet<String>,
}

impl StdioProtocolState {
    fn observe_inbound(
        &mut self,
        value: &serde_json::Value,
        modern_downstream_enabled: bool,
    ) -> bool {
        if !modern_downstream_enabled {
            self.modern_confirmed = false;
            self.pending_discovery_ids.clear();
            self.pending_modern_initialize_ids.clear();
            return false;
        }

        let method = value.get("method").and_then(serde_json::Value::as_str);
        let explicit_legacy_initialize = method == Some("initialize")
            && value
                .pointer("/params/protocolVersion")
                .and_then(serde_json::Value::as_str)
                != Some(plug_core::protocol::MODERN_PROTOCOL_VERSION);
        if explicit_legacy_initialize {
            self.modern_confirmed = false;
            self.pending_discovery_ids.clear();
            self.pending_modern_initialize_ids.clear();
            return false;
        }

        if method == Some("server/discover") {
            if let Some(id) = value.get("id") {
                self.pending_discovery_ids.insert(id.to_string());
            }
        } else if method == Some("initialize") {
            if let Some(id) = value.get("id") {
                self.pending_modern_initialize_ids.insert(id.to_string());
            }
        } else if is_modern_stdio_message(value) {
            self.modern_confirmed = true;
        }
        self.modern_confirmed
    }

    fn observe_outbound(&mut self, value: &serde_json::Value) -> bool {
        if let Some(id) = value.get("id") {
            let id = id.to_string();
            let discovered = self.pending_discovery_ids.remove(&id)
                && value.get("result").is_some()
                && value.get("error").is_none();
            let negotiated_modern = self.pending_modern_initialize_ids.remove(&id)
                && value
                    .pointer("/result/protocolVersion")
                    .and_then(serde_json::Value::as_str)
                    == Some(plug_core::protocol::MODERN_PROTOCOL_VERSION);
            if discovered || negotiated_modern {
                self.modern_confirmed = true;
            }
        }
        self.modern_confirmed
    }
}

/// Byte-level protocol adapter. Legacy sessions retain the exact SEP-1686
/// request/response vocabulary; a gated modern session skips those rewrites and
/// has `resultType` completed on every result, beginning with `server/discover`
/// or an `initialize` that negotiates `2026-07-28` as its first message.
pub(super) fn stdio_transport(
    modern_gate: Arc<dyn Fn() -> bool + Send + Sync>,
) -> tokio::io::DuplexStream {
    bridge_transport(tokio::io::stdin(), tokio::io::stdout(), modern_gate)
}

fn bridge_transport<I, O>(
    input: I,
    mut output: O,
    modern_gate: Arc<dyn Fn() -> bool + Send + Sync>,
) -> tokio::io::DuplexStream
where
    I: tokio::io::AsyncRead + Unpin + Send + 'static,
    O: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    let (service, bridge) = tokio::io::duplex(256 * 1024);
    let (bridge_read, mut bridge_write) = tokio::io::split(bridge);
    let task_requests = std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::<
        String,
    >::new()));
    let protocol_state = std::sync::Arc::new(std::sync::Mutex::new(StdioProtocolState::default()));

    let inbound_tasks = std::sync::Arc::clone(&task_requests);
    let inbound_protocol = std::sync::Arc::clone(&protocol_state);
    tokio::spawn(async move {
        let mut input = BufReader::new(input).lines();
        while let Ok(Some(line)) = input.next_line().await {
            let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&line) else {
                let _ = bridge_write.write_all(line.as_bytes()).await;
                let _ = bridge_write.write_all(b"\n").await;
                continue;
            };
            let modern_downstream_enabled = modern_gate();
            let is_modern = inbound_protocol
                .lock()
                .map(|mut state| state.observe_inbound(&value, modern_downstream_enabled))
                .unwrap_or(false);
            let task_request = !is_modern
                && value.get("method").and_then(serde_json::Value::as_str) == Some("tools/call")
                && value
                    .get("params")
                    .and_then(|params| params.get("task"))
                    .is_some();
            if task_request && let Some(id) = value.get("id") {
                inbound_tasks.lock().await.insert(id.to_string());
            }
            if !is_modern {
                plug_core::protocol::rewrite_legacy_request(&mut value);
            }
            if let Ok(mut encoded) = serde_json::to_vec(&value) {
                encoded.push(b'\n');
                if bridge_write.write_all(&encoded).await.is_err() {
                    break;
                }
            }
        }
        // Dropping one half of a split duplex does not close it; without an
        // explicit shutdown the service never sees EOF and the process stays
        // up after the host closes stdin.
        let _ = bridge_write.shutdown().await;
    });

    let outbound_protocol = protocol_state;
    tokio::spawn(async move {
        let mut lines = BufReader::new(bridge_read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&line) else {
                let _ = output.write_all(line.as_bytes()).await;
                let _ = output.write_all(b"\n").await;
                continue;
            };
            let is_modern = outbound_protocol
                .lock()
                .map(|mut state| state.observe_outbound(&value))
                .unwrap_or(false);
            let task_response = if !is_modern && let Some(id) = value.get("id") {
                task_requests.lock().await.remove(&id.to_string())
            } else {
                false
            };
            if is_modern {
                plug_core::protocol::rewrite_modern_response(&mut value);
            } else {
                plug_core::protocol::rewrite_legacy_response(&mut value, task_response);
            }
            if let Ok(mut encoded) = serde_json::to_vec(&value) {
                encoded.push(b'\n');
                if output.write_all(&encoded).await.is_err() {
                    break;
                }
                let _ = output.flush().await;
            }
        }
    });

    service
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    #[test]
    fn failed_modern_discovery_probe_falls_back_to_explicit_legacy_initialize() {
        let mut protocol = StdioProtocolState::default();
        let discovery = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "server/discover",
            "params": {}
        });
        assert!(!protocol.observe_inbound(&discovery, true));
        assert!(!protocol.observe_outbound(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "error": {"code": -32601, "message": "not supported"}
        })));

        let mut initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {"tasks": {"list": {}}},
                "clientInfo": {"name": "legacy-after-probe", "version": "1.0"}
            }
        });
        assert!(!protocol.observe_inbound(&initialize, true));
        plug_core::protocol::rewrite_legacy_request(&mut initialize);
        assert!(initialize.pointer("/params/capabilities/tasks").is_none());
        assert!(
            initialize
                .pointer("/params/capabilities/experimental/plug.dev~1legacy-tasks")
                .is_some()
        );

        let mut response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 8,
            "result": {"resultType": "complete", "tools": []}
        });
        assert!(!protocol.observe_outbound(&response));
        plug_core::protocol::rewrite_legacy_response(&mut response, false);
        assert!(response.pointer("/result/resultType").is_none());
    }

    #[test]
    fn successful_modern_discovery_confirms_modern_stdio_era() {
        let mut protocol = StdioProtocolState::default();
        let discovery = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "server/discover",
            "params": {}
        });
        assert!(!protocol.observe_inbound(&discovery, true));
        assert!(protocol.observe_outbound(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 9,
            "result": {"supportedVersions": ["2026-07-28"]}
        })));
        assert!(
            !protocol.observe_inbound(
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 10,
                    "method": "tools/list",
                    "params": {
                        "_meta": {
                            "io.modelcontextprotocol/protocolVersion": "2026-07-28"
                        }
                    }
                }),
                false,
            ),
            "a live gate disable must clear the confirmed modern era"
        );
    }

    #[test]
    fn modern_initialize_confirms_the_era_only_when_the_reply_negotiates_it() {
        let initialize = |id: u64| {
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "initialize",
                "params": {"protocolVersion": "2026-07-28"}
            })
        };
        let mut protocol = StdioProtocolState::default();
        assert!(!protocol.observe_inbound(&initialize(1), true));
        assert!(!protocol.observe_outbound(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"protocolVersion": "2025-11-25"}
        })));
        assert!(!protocol.observe_inbound(&initialize(2), true));
        assert!(protocol.observe_outbound(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {"protocolVersion": "2026-07-28"}
        })));
        assert!(
            !protocol.observe_inbound(&initialize(3), false),
            "a live gate disable must clear the confirmed modern era"
        );
    }

    /// Host and service ends of a `bridge_transport`, driven line by line so a
    /// test sees exactly what crosses the stdio seam in each direction.
    struct BridgeHarness {
        host_in: tokio::io::DuplexStream,
        host_out: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
        service_in: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
        service_out: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    }

    impl BridgeHarness {
        fn new(modern_gate: bool) -> Self {
            let (host_in, bridge_input) = tokio::io::duplex(64 * 1024);
            let (bridge_output, host_out) = tokio::io::duplex(64 * 1024);
            let service =
                bridge_transport(bridge_input, bridge_output, Arc::new(move || modern_gate));
            let (service_read, service_out) = tokio::io::split(service);
            Self {
                host_in,
                host_out: BufReader::new(host_out).lines(),
                service_in: BufReader::new(service_read).lines(),
                service_out,
            }
        }

        /// Send `request` from the host, have the service answer it with
        /// `reply_body` (`{"result": ..}` or `{"error": ..}`), and return what
        /// the host receives back.
        async fn round_trip(
            &mut self,
            request: serde_json::Value,
            reply_body: serde_json::Value,
        ) -> serde_json::Value {
            let step = Duration::from_secs(5);
            let mut line = serde_json::to_vec(&request).expect("encode request");
            line.push(b'\n');
            self.host_in.write_all(&line).await.expect("host write");
            tokio::time::timeout(step, self.service_in.next_line())
                .await
                .expect("service receives the forwarded request")
                .expect("service read")
                .expect("service line");
            let mut reply = serde_json::json!({"jsonrpc": "2.0", "id": request["id"]});
            reply
                .as_object_mut()
                .expect("reply object")
                .extend(reply_body.as_object().expect("reply body object").clone());
            let mut line = serde_json::to_vec(&reply).expect("encode reply");
            line.push(b'\n');
            self.service_out
                .write_all(&line)
                .await
                .expect("service write");
            let line = tokio::time::timeout(step, self.host_out.next_line())
                .await
                .expect("host receives the reply")
                .expect("host read")
                .expect("host line");
            serde_json::from_str(&line).expect("reply is JSON")
        }
    }

    const MODERN_META_KEY: &str = "io.modelcontextprotocol/protocolVersion";

    fn modern_tools_call(id: u64) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "Context7__resolve_library_id",
                "arguments": {},
                "_meta": {MODERN_META_KEY: "2026-07-28"}
            }
        })
    }

    fn proxied_tool_result() -> serde_json::Value {
        serde_json::json!({"result": {"content": [], "isError": false}})
    }

    #[tokio::test]
    async fn stdio_modern_discovery_session_completes_proxied_tool_results() {
        let mut bridge = BridgeHarness::new(true);
        let discovered = bridge
            .round_trip(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "server/discover",
                    "params": {}
                }),
                serde_json::json!({"result": {
                    "resultType": "complete",
                    "supportedVersions": ["2026-07-28"]
                }}),
            )
            .await;
        assert_eq!(discovered["result"]["resultType"], "complete");

        let called = bridge
            .round_trip(modern_tools_call(2), proxied_tool_result())
            .await;
        assert_eq!(called["result"]["resultType"], "complete");
        assert_eq!(called["result"]["isError"], false);
    }

    #[tokio::test]
    async fn stdio_plain_initialize_at_modern_version_completes_proxied_tool_results() {
        let mut bridge = BridgeHarness::new(true);
        let initialized = bridge
            .round_trip(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2026-07-28",
                        "capabilities": {},
                        "clientInfo": {"name": "modern-initialize", "version": "1.0"}
                    }
                }),
                serde_json::json!({"result": {
                    "protocolVersion": "2026-07-28",
                    "capabilities": {},
                    "serverInfo": {"name": "plug", "version": "0"}
                }}),
            )
            .await;
        assert_eq!(initialized["result"]["protocolVersion"], "2026-07-28");
        assert_eq!(initialized["result"]["resultType"], "complete");

        // A plain-initialize client carries no per-request protocol metadata.
        let called = bridge
            .round_trip(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {"name": "Context7__resolve_library_id", "arguments": {}}
                }),
                proxied_tool_result(),
            )
            .await;
        assert_eq!(called["result"]["resultType"], "complete");
    }

    #[tokio::test]
    async fn stdio_modern_session_keeps_input_required_and_errors_unchanged() {
        let mut bridge = BridgeHarness::new(true);
        bridge
            .round_trip(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "server/discover",
                    "params": {}
                }),
                serde_json::json!({"result": {"supportedVersions": ["2026-07-28"]}}),
            )
            .await;

        let input = bridge
            .round_trip(
                modern_tools_call(2),
                serde_json::json!({"result": {"resultType": "input_required"}}),
            )
            .await;
        assert_eq!(input["result"]["resultType"], "input_required");

        let failed = bridge
            .round_trip(
                modern_tools_call(3),
                serde_json::json!({"error": {"code": -32602, "message": "bad"}}),
            )
            .await;
        assert_eq!(failed["error"]["code"], -32602);
        assert!(failed.get("result").is_none());
    }

    #[tokio::test]
    async fn stdio_legacy_initialize_session_still_omits_result_type() {
        let mut bridge = BridgeHarness::new(true);
        let initialized = bridge
            .round_trip(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "clientInfo": {"name": "legacy", "version": "1.0"}
                    }
                }),
                serde_json::json!({"result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "serverInfo": {"name": "plug", "version": "0"}
                }}),
            )
            .await;
        assert!(initialized["result"].get("resultType").is_none());

        let called = bridge
            .round_trip(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {"name": "Context7__resolve_library_id", "arguments": {}}
                }),
                serde_json::json!({"result": {
                    "resultType": "complete",
                    "content": [],
                    "isError": false
                }}),
            )
            .await;
        assert!(called["result"].get("resultType").is_none());
        assert_eq!(called["result"]["isError"], false);
    }

    #[tokio::test]
    async fn stdio_modern_initialize_refused_by_the_daemon_stays_legacy() {
        let mut bridge = BridgeHarness::new(true);
        let refused = bridge
            .round_trip(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2026-07-28",
                        "capabilities": {},
                        "clientInfo": {"name": "refused", "version": "1.0"}
                    }
                }),
                serde_json::json!({"error": {"code": -32602, "message": "unsupported"}}),
            )
            .await;
        assert_eq!(refused["error"]["code"], -32602);

        let called = bridge
            .round_trip(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {"name": "Context7__resolve_library_id", "arguments": {}}
                }),
                serde_json::json!({"result": {"resultType": "complete", "content": []}}),
            )
            .await;
        assert!(called["result"].get("resultType").is_none());
    }

    #[tokio::test]
    async fn bridge_closes_the_service_side_when_the_host_closes_stdin() {
        use tokio::io::AsyncReadExt as _;

        let mut service =
            bridge_transport(tokio::io::empty(), tokio::io::sink(), Arc::new(|| false));
        let mut buf = [0u8; 16];
        let read = tokio::time::timeout(Duration::from_secs(5), service.read(&mut buf))
            .await
            .expect("service side sees EOF once stdin closes")
            .expect("read succeeds");
        assert_eq!(read, 0);
    }
}
