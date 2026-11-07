//! Where Plug serves its OAuth authorization server and sign-in pages. The
//! router, the issuer's metadata, the pages and their scripts (through the
//! data attributes each page carries) all read these.

/// The protected resource.
pub const MCP: &str = "/mcp";
pub const AUTHORIZATION_SERVER_METADATA: &str = "/.well-known/oauth-authorization-server";
pub const PROTECTED_RESOURCE_METADATA: &str = "/.well-known/oauth-protected-resource";
/// The path-suffixed form RFC 9728 gives a resource at `/mcp`.
pub const PROTECTED_RESOURCE_METADATA_FOR_MCP: &str = "/.well-known/oauth-protected-resource/mcp";
pub const REGISTER: &str = "/oauth/register";
pub const AUTHORIZE: &str = "/oauth/authorize";
pub const TOKEN: &str = "/oauth/token";
/// A client that renews before it has read the metadata again posts to the
/// address older MCP clients assume. Without it a stored sign-in cannot renew
/// after the client restarts.
pub const ASSUMED_TOKEN: &str = "/token";
pub const CONSENT_CHALLENGE: &str = "/oauth/consent/challenge";
pub const CONSENT_DECISION: &str = "/oauth/consent/decision";
pub const OWNER_ENROLL: &str = "/oauth/owner/enroll";
pub const OWNER_ENROLL_CHALLENGE: &str = "/oauth/owner/enroll/challenge";
pub const OWNER_ENROLL_COMPLETE: &str = "/oauth/owner/enroll/complete";
pub const CONSENT_SCRIPT: &str = "/oauth/assets/consent.js";
pub const ENROLL_SCRIPT: &str = "/oauth/assets/enroll.js";
pub const STYLESHEET: &str = "/oauth/assets/plug.css";
pub const ICON: &str = "/oauth/assets/plug-icon.svg";
