pub mod sse_client;

/// Whether an upstream that refused a request with HTTP `status` and the
/// `WWW-Authenticate` value `challenge` wants its owner to sign in again.
///
/// The MCP authorization spec (2025-06-18 and 2025-11-25, "Error Handling")
/// says a 401 means authorization is required or the token is invalid, and a
/// 403 means the token lacks scopes or permissions. A 403 whose challenge
/// says `error="insufficient_scope"` asks for step-up authorization: a new
/// sign-in that grants more. Any other 403 is the server saying no to this
/// account, which signing in again does not change.
pub(crate) fn refusal_needs_sign_in(status: u16, challenge: Option<&str>) -> bool {
    match status {
        401 => true,
        403 => challenge.is_some_and(|challenge| {
            challenge
                .to_ascii_lowercase()
                .contains("insufficient_scope")
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::refusal_needs_sign_in;

    #[test]
    fn a_401_or_a_403_for_more_scope_needs_sign_in_and_a_plain_403_does_not() {
        assert!(refusal_needs_sign_in(401, None));
        assert!(refusal_needs_sign_in(
            401,
            Some(r#"Bearer error="invalid_token""#)
        ));
        assert!(refusal_needs_sign_in(
            403,
            Some(r#"Bearer error="insufficient_scope", scope="files:write""#)
        ));
        assert!(!refusal_needs_sign_in(403, None));
        assert!(!refusal_needs_sign_in(403, Some(r#"Bearer realm="mcp""#)));
        assert!(!refusal_needs_sign_in(500, None));
    }
}
