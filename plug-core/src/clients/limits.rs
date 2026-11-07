//! Published ceilings on how many MCP tools one client accepts at once.
//!
//! These are other products' limits, not plug's. They move when those
//! products move, and a number compiled in here goes stale silently: nothing
//! fails, plug just starts asserting something untrue. That already happened
//! once: the table shipped on 2026-03-04 with `("Cursor", 40)` and was still
//! claiming it five months later, long after Cursor had dropped the cap.
//!
//! So every entry carries the date it was last checked and the page that was
//! read. Re-verify before trusting one, and move `verified` when you do. A
//! client that publishes no fixed ceiling does not belong in this table:
//! omitting it is the accurate answer, and inventing a number to fill the row
//! is how the Cursor entry got there.
//!
//! Deliberately absent, each confirmed 2026-08-08:
//! - **Cursor.** Older versions warned above roughly 40-50 tools. Current
//!   versions use Dynamic Context Discovery and publish no ceiling;
//!   <https://cursor.com/docs/context/mcp> states no limit.
//! - **Claude Code, Codex CLI, Grok Build.** No published ceiling.
//! - **The Devin CLI and Devin Desktop's default agent**, which read the file
//!   the `devin` target writes, publish none;
//!   <https://docs.devin.ai/cli/extensibility/mcp/configuration> (2026-10-03).

use super::ClientTarget;
use crate::types::ClientType;

/// A published ceiling on how many MCP tools one client will accept at once.
pub struct ClientToolLimit {
    /// The client, as Plug recognises it on a live session. Its sessions are
    /// served only the first `limit` tools while `tool_filter_enabled` is on.
    pub client: ClientType,
    /// The link target whose settings file reaches this client, so a linked
    /// target means the client uses Plug. `None` when linking says nothing
    /// about it.
    pub linked_by: Option<ClientTarget>,
    pub limit: usize,
    /// Date this entry was last checked against `source`, `YYYY-MM-DD`.
    pub verified: &'static str,
    /// The page that was read. Primary vendor documentation where one exists.
    pub source: &'static str,
}

pub const TOOL_LIMITS: &[ClientToolLimit] = &[
    ClientToolLimit {
        // Cascade, Devin Desktop's legacy agent, still introduces itself as
        // `windsurf-client`, which Plug takes for Devin. It reads the old
        // Windsurf file that plug no longer writes, so a linked `devin`
        // target says nothing about it.
        client: ClientType::Devin,
        linked_by: None,
        limit: 100,
        verified: "2026-10-03",
        source: "https://docs.devin.ai/desktop/cascade/mcp",
    },
    ClientToolLimit {
        client: ClientType::VSCodeCopilot,
        linked_by: Some(ClientTarget::VSCodeCopilot),
        limit: 128,
        verified: "2026-08-30",
        // Hard cap enforced per request; `github.copilot.chat.virtualTools.threshold`
        // itself caps at 128, and extra tools are deferred behind `activate_*` stubs.
        source: "https://github.com/microsoft/vscode/issues/290356",
    },
];

impl ClientType {
    /// The most tools this client accepts, when it publishes a ceiling.
    pub fn tool_limit(&self) -> Option<usize> {
        TOOL_LIMITS
            .iter()
            .find(|entry| entry.client == *self)
            .map(|entry| entry.limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards the rule the Cursor entry broke: a client with no published
    /// ceiling must not be given an invented one.
    #[test]
    fn no_client_limit_is_asserted_without_a_dated_source() {
        for entry in TOOL_LIMITS {
            assert!(
                entry.source.starts_with("https://"),
                "{} needs a source URL that can be re-checked",
                entry.client
            );
            let parts: Vec<&str> = entry.verified.split('-').collect();
            assert!(
                parts.len() == 3 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())),
                "{} needs a YYYY-MM-DD verified date, got {:?}",
                entry.client,
                entry.verified
            );
            assert!(entry.limit > 0, "{} has a meaningless limit", entry.client);
        }
    }
}
