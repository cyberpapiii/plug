//! Which lazy tool discovery a client gets: what `[lazy_tools]` sets for it,
//! else what its own capabilities call for.

use super::ClientTarget;
use crate::config::LazyToolsConfig;
use crate::types::{
    ClientType, LazyToolMode, LazyToolModeOrigin, LazyToolSetting, ResolvedLazyToolPolicy,
};

/// The policy for the client at `target`, a client slug. A key under
/// `[lazy_tools.clients]` may be the slug or one of its aliases.
pub fn lazy_tool_policy(
    meta_tool_mode: bool,
    lazy_tools: &LazyToolsConfig,
    target: &str,
) -> ResolvedLazyToolPolicy {
    let client = ClientTarget::from_slug(target);
    let set_for_client = lazy_tools.clients.get(target).or_else(|| {
        lazy_tools
            .clients
            .iter()
            .find(|(key, _)| client.is_some() && ClientTarget::from_slug(key) == client)
            .map(|(_, setting)| setting)
    });
    if let Some(mode) = set_for_client.and_then(|setting| mode_of(*setting)) {
        return resolved(mode, LazyToolModeOrigin::ClientOverride);
    }
    if let Some(mode) = mode_of(lazy_tools.mode) {
        return resolved(mode, LazyToolModeOrigin::GlobalOverride);
    }
    if meta_tool_mode {
        return resolved(LazyToolMode::Bridge, LazyToolModeOrigin::LegacyMetaToolMode);
    }
    let mode = client
        .and_then(|target| target.info().live)
        .map_or(LazyToolMode::Standard, default_mode);
    resolved(mode, LazyToolModeOrigin::AutoDefault)
}

fn default_mode(client_type: ClientType) -> LazyToolMode {
    match client_type {
        ClientType::ClaudeCode | ClientType::Cursor | ClientType::CodexCli => LazyToolMode::Native,
        ClientType::OpenCode => LazyToolMode::Bridge,
        ClientType::ClaudeDesktop
        | ClientType::Devin
        | ClientType::VSCodeCopilot
        | ClientType::CopilotCli
        | ClientType::GeminiCli
        | ClientType::GrokBuild
        | ClientType::GrokBot
        | ClientType::Zed
        | ClientType::Unknown => LazyToolMode::Standard,
    }
}

fn mode_of(setting: LazyToolSetting) -> Option<LazyToolMode> {
    match setting {
        LazyToolSetting::Auto => None,
        LazyToolSetting::Standard => Some(LazyToolMode::Standard),
        LazyToolSetting::Native => Some(LazyToolMode::Native),
        LazyToolSetting::Bridge => Some(LazyToolMode::Bridge),
    }
}

fn resolved(mode: LazyToolMode, origin: LazyToolModeOrigin) -> ResolvedLazyToolPolicy {
    let reason = match origin {
        LazyToolModeOrigin::ClientOverride => "configured for this client target",
        LazyToolModeOrigin::GlobalOverride => "configured globally for all client targets",
        LazyToolModeOrigin::LegacyMetaToolMode => "mapped from deprecated meta_tool_mode = true",
        LazyToolModeOrigin::AutoDefault => match mode {
            LazyToolMode::Standard => {
                "default full-tool behavior for clients without a proven lazy discovery path"
            }
            LazyToolMode::Native => "client has its own native lazy/deferred tool discovery",
            LazyToolMode::Bridge => "client benefits from plug-owned search bridge discovery",
        },
    };
    ResolvedLazyToolPolicy::new(mode, origin, reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lazy(mode: LazyToolSetting, clients: &[(&str, LazyToolSetting)]) -> LazyToolsConfig {
        LazyToolsConfig {
            mode,
            clients: clients
                .iter()
                .map(|(key, setting)| (key.to_string(), *setting))
                .collect(),
        }
    }

    #[test]
    fn a_client_setting_beats_the_global_one_for_that_client_only() {
        let lazy = lazy(
            LazyToolSetting::Standard,
            &[("opencode", LazyToolSetting::Bridge)],
        );
        let opencode = lazy_tool_policy(false, &lazy, "opencode");
        assert_eq!(opencode.mode, LazyToolMode::Bridge);
        assert_eq!(opencode.origin, LazyToolModeOrigin::ClientOverride);
        let cursor = lazy_tool_policy(false, &lazy, "cursor");
        assert_eq!(cursor.mode, LazyToolMode::Standard);
        assert_eq!(cursor.origin, LazyToolModeOrigin::GlobalOverride);
    }

    #[test]
    fn auto_follows_what_the_client_can_do() {
        let lazy = LazyToolsConfig::default();
        let cases = [
            ("claude-code", LazyToolMode::Native),
            ("opencode", LazyToolMode::Bridge),
            ("unknown-client", LazyToolMode::Standard),
        ];
        for (target, mode) in cases {
            let policy = lazy_tool_policy(false, &lazy, target);
            assert_eq!(
                (policy.mode, policy.origin),
                (mode, LazyToolModeOrigin::AutoDefault)
            );
        }
    }

    #[test]
    fn meta_tool_mode_means_the_bridge_unless_something_is_set() {
        let policy = lazy_tool_policy(true, &LazyToolsConfig::default(), "claude-desktop");
        assert_eq!(policy.mode, LazyToolMode::Bridge);
        assert_eq!(policy.origin, LazyToolModeOrigin::LegacyMetaToolMode);
    }

    #[test]
    fn an_alias_key_sets_its_client_and_an_unknown_key_sets_none() {
        let lazy = lazy(
            LazyToolSetting::Auto,
            &[
                ("codex", LazyToolSetting::Standard),
                ("not-a-client", LazyToolSetting::Bridge),
            ],
        );
        let codex = lazy_tool_policy(false, &lazy, "codex-cli");
        assert_eq!(codex.mode, LazyToolMode::Standard);
        assert_eq!(codex.origin, LazyToolModeOrigin::ClientOverride);
        let cursor = lazy_tool_policy(false, &lazy, "cursor");
        assert_eq!(cursor.origin, LazyToolModeOrigin::AutoDefault);
    }
}
