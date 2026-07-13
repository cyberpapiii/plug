//! Protocol-version policy shared by Plug's downstream transports.

use rmcp::ErrorData as McpError;
use rmcp::model::ProtocolVersion;

pub const SUPPORTED_PROTOCOL_VERSION: &str = "2025-11-25";
const ANNOUNCED_FUTURE_PROTOCOL_VERSION: &str = "2026-07-28";

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
}
