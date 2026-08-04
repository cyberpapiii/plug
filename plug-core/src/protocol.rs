//! Protocol-version policy shared by Plug's downstream transports.

use rmcp::ErrorData as McpError;
use rmcp::model::ProtocolVersion;

pub const SUPPORTED_PROTOCOL_VERSION: &str = "2025-11-25";
pub const ANNOUNCED_FUTURE_PROTOCOL_VERSION: &str = "2026-07-28";

/// Parse the single protocol revision Plug is currently prepared to serve.
/// Keeping construction here prevents individual transports from drifting.
pub fn supported_protocol_version() -> ProtocolVersion {
    serde_json::from_value(serde_json::Value::String(
        SUPPORTED_PROTOCOL_VERSION.to_string(),
    ))
    .expect("Plug's supported protocol version must parse")
}

pub const LEGACY_TASKS_CAPABILITY_KEY: &str = "plug.dev/legacy-tasks";
pub const LEGACY_TASK_REQUEST_KEY: &str = "plug.dev/legacy-task";

pub fn legacy_tasks_capability(capabilities: &rmcp::model::ServerCapabilities) -> bool {
    capabilities
        .experimental
        .as_ref()
        .is_some_and(|value| value.contains_key(LEGACY_TASKS_CAPABILITY_KEY))
}

/// Rewrite removed SEP-1686 task syntax into private extension syntax before
/// RMCP 3.x performs typed deserialization. Adapter output performs the inverse.
pub fn rewrite_legacy_request(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let method = object
        .get("method")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if let Some(method) = method {
        if matches!(
            method.as_str(),
            "tasks/list" | "tasks/get" | "tasks/result" | "tasks/cancel"
        ) {
            object.insert(
                "method".into(),
                serde_json::Value::String(format!("plug/legacy/{method}")),
            );
        }
        if method == "tools/call"
            && let Some(params) = object.get_mut("params")
        {
            rewrite_legacy_call_params(params);
        }
        if method == "initialize"
            && let Some(capabilities) = object
                .get_mut("params")
                .and_then(|value| value.get_mut("capabilities"))
                .and_then(serde_json::Value::as_object_mut)
            && let Some(tasks) = capabilities.remove("tasks")
        {
            capabilities
                .entry("extensions")
                .or_insert_with(|| serde_json::json!({}))[rmcp::model::TASKS_EXTENSION_ID] =
                serde_json::json!({});
            capabilities
                .entry("experimental")
                .or_insert_with(|| serde_json::json!({}))[LEGACY_TASKS_CAPABILITY_KEY] = tasks;
        }
    }
}

/// Rewrite the removed SEP-1686 `task` parameter on a bare tools/call params
/// object. The HTTP and stdio adapters call [`rewrite_legacy_request`] on a
/// complete JSON-RPC envelope; daemon IPC carries params without that envelope.
pub fn rewrite_legacy_call_params(value: &mut serde_json::Value) {
    let Some(params) = value.as_object_mut() else {
        return;
    };
    if let Some(task) = params.remove("task") {
        params
            .entry("_meta")
            .or_insert_with(|| serde_json::json!({}))[LEGACY_TASK_REQUEST_KEY] = task;
    }
}

/// Restore the pre-3.x wire vocabulary after RMCP has produced a response.
pub fn rewrite_legacy_response(value: &mut serde_json::Value, task_response: bool) {
    let Some(result) = value.get_mut("result") else {
        return;
    };
    rewrite_legacy_result(result, task_response);
}

/// Restore the pre-3.x vocabulary on a bare MCP result payload.
///
/// HTTP responses wrap this payload in JSON-RPC and use
/// [`rewrite_legacy_response`]. The daemon's internal IPC MCP bridge carries the
/// result object directly, so it needs the same conversion without an envelope.
pub fn rewrite_legacy_result(value: &mut serde_json::Value, task_response: bool) {
    let Some(result) = value.as_object_mut() else {
        return;
    };

    // `resultType` was introduced by the 2026 lifecycle. RMCP 3.x populates it
    // on paginated results even while Plug deliberately negotiates 2025-11-25.
    // Never expose that modern-only discriminator on a legacy connection.
    result.remove("resultType");

    if task_response
        && let Some(task) = result
            .get_mut("task")
            .and_then(serde_json::Value::as_object_mut)
    {
        if let Some(value) = task.remove("ttlMs") {
            task.insert("ttl".into(), value);
        }
        if let Some(value) = task.remove("pollIntervalMs") {
            task.insert("pollInterval".into(), value);
        }
    }
    if let Some(capabilities) = result
        .get_mut("capabilities")
        .and_then(serde_json::Value::as_object_mut)
    {
        let (tasks, remove_experimental) = capabilities
            .get_mut("experimental")
            .and_then(serde_json::Value::as_object_mut)
            .map(|experimental| {
                let tasks = experimental.remove(LEGACY_TASKS_CAPABILITY_KEY);
                (tasks, experimental.is_empty())
            })
            .unwrap_or((None, false));
        if let Some(tasks) = tasks {
            capabilities.insert("tasks".into(), tasks);
        }
        if remove_experimental {
            capabilities.remove("experimental");
        }
    }
}

/// Reject the announced future revision that RMCP 2.2 knows how to name but
/// Plug does not implement yet. Older and unknown versions retain RMCP's
/// existing negotiation behavior; only the known-unimplemented revision is
/// blocked before RMCP can echo it as accepted.
pub fn ensure_supported_downstream_protocol(requested: &ProtocolVersion) -> Result<(), McpError> {
    if requested.as_str() == ANNOUNCED_FUTURE_PROTOCOL_VERSION {
        return Err(McpError::invalid_params(
            format!(
                "MCP protocol version {ANNOUNCED_FUTURE_PROTOCOL_VERSION} is not supported; latest supported version is {SUPPORTED_PROTOCOL_VERSION}"
            ),
            None,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_announced_revision_before_rmcp_can_echo_it() {
        let error = ensure_supported_downstream_protocol(&ProtocolVersion::V_2026_07_28)
            .expect_err("future protocol must be rejected");
        assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(error.message.contains(SUPPORTED_PROTOCOL_VERSION));
    }

    #[test]
    fn accepts_current_stable_revision() {
        ensure_supported_downstream_protocol(&ProtocolVersion::V_2025_11_25)
            .expect("current stable protocol must be accepted");
    }

    #[test]
    fn parsed_supported_version_matches_policy_literal() {
        assert_eq!(
            supported_protocol_version().as_str(),
            SUPPORTED_PROTOCOL_VERSION
        );
    }
}
