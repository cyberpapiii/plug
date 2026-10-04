//! Skills served over MCP keep their server's name.
//!
//! A skill is a directory of resources under `skill://<skill-path>/…`, and
//! its URI is scoped to the server that serves it: two servers may both
//! serve `skill://refunds/SKILL.md`. Plug puts the server's name in front
//! of the path, so `skill://refunds/SKILL.md` from `acme` is
//! `skill://acme/refunds/SKILL.md` to every client, and takes it off again
//! before it asks the server. The last segment of the skill path is still
//! the skill's name, so the URI stays valid; file bytes are never touched.
//!
//! A skill under another scheme keeps its URI: it is already its server's.

use std::borrow::Cow;
use std::collections::HashMap;

use rmcp::model::{CallToolResult, ContentBlock, ResourceContents};

const SCHEME: &str = "skill://";

/// `uri` as a client sees it when `server` serves it.
pub(crate) fn outward<'a>(server: &str, uri: &'a str) -> Cow<'a, str> {
    match uri.strip_prefix(SCHEME) {
        Some(path) => Cow::Owned(format!("{SCHEME}{server}/{path}")),
        None => Cow::Borrowed(uri),
    }
}

/// `uri` as `server` knows it.
pub(crate) fn inward<'a>(server: &str, uri: &'a str) -> Cow<'a, str> {
    uri.strip_prefix(SCHEME)
        .and_then(|rest| rest.strip_prefix(server))
        .and_then(|rest| rest.strip_prefix('/'))
        .map_or(Cow::Borrowed(uri), |path| {
            Cow::Owned(format!("{SCHEME}{path}"))
        })
}

/// The server a skill URI names in its first segment. A skill's files need
/// not be listed, so a read is routed by this when no listed route matches.
pub(crate) fn named_server(uri: &str) -> Option<&str> {
    let (server, _) = uri.strip_prefix(SCHEME)?.split_once('/')?;
    (!server.is_empty()).then_some(server)
}

/// The one server that lists `uri` when a client asks for it without the
/// server's name, as a server's own instructions would write it.
pub(crate) fn only_server(routes: &HashMap<String, String>, uri: &str) -> Option<String> {
    let path = uri.strip_prefix(SCHEME)?;
    let mut servers = routes.iter().filter_map(|(route, server)| {
        (inward(server, route).strip_prefix(SCHEME) == Some(path) && route != uri).then_some(server)
    });
    let first = servers.next()?;
    servers.all(|server| server == first).then(|| first.clone())
}

fn rewrite(server: &str, uri: &mut String) {
    if let Cow::Owned(named) = outward(server, uri) {
        *uri = named;
    }
}

pub(crate) fn outward_contents(server: &str, contents: &mut ResourceContents) {
    match contents {
        ResourceContents::TextResourceContents { uri, .. }
        | ResourceContents::BlobResourceContents { uri, .. } => rewrite(server, uri),
        _ => {}
    }
}

/// Name `server` in every skill a tool result links to or embeds.
pub(crate) fn outward_tool_result(server: &str, result: &mut CallToolResult) {
    for block in &mut result.content {
        match block {
            ContentBlock::Resource(embedded) => outward_contents(server, &mut embedded.resource),
            ContentBlock::ResourceLink(link) => rewrite(server, &mut link.uri),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_skill_uri_gains_its_server_and_loses_it_again() {
        let named = outward("acme", "skill://refunds/SKILL.md");
        assert_eq!(named, "skill://acme/refunds/SKILL.md");
        assert_eq!(inward("acme", &named), "skill://refunds/SKILL.md");
        assert_eq!(named_server(&named), Some("acme"));
    }

    #[test]
    fn only_the_owning_server_is_taken_off_and_other_schemes_are_left() {
        assert_eq!(inward("acme", "skill://acme-two/x"), "skill://acme-two/x");
        assert_eq!(inward("other", "skill://acme/x"), "skill://acme/x");
        assert_eq!(
            outward("acme", "github://o/r/SKILL.md"),
            "github://o/r/SKILL.md"
        );
        assert_eq!(outward("acme", "file:///a"), "file:///a");
        assert_eq!(named_server("file:///a"), None);
    }

    #[test]
    fn a_bare_skill_uri_finds_its_server_only_when_one_serves_it() {
        let mut routes = HashMap::from([(
            "skill://acme/refunds/SKILL.md".to_string(),
            "acme".to_string(),
        )]);
        assert_eq!(
            only_server(&routes, "skill://refunds/SKILL.md").as_deref(),
            Some("acme")
        );
        routes.insert(
            "skill://other/refunds/SKILL.md".to_string(),
            "other".to_string(),
        );
        assert_eq!(only_server(&routes, "skill://refunds/SKILL.md"), None);
        assert_eq!(only_server(&routes, "skill://missing/SKILL.md"), None);
    }
}
