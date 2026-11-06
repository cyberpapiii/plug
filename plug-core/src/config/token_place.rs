//! `token_in`: where an openapi server sends its `auth_token`.

/// Where a request carries the server's `auth_token`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenPlace {
    /// `Authorization: Bearer <token>`.
    Bearer,
    /// A header of this name holding the token alone.
    Header(String),
    /// A query parameter of this name.
    Query(String),
}

impl TokenPlace {
    /// Read the `token_in` setting: `bearer`, `header:<name>`, or
    /// `query:<name>`.
    pub fn parse(setting: &str) -> anyhow::Result<Self> {
        let place = match setting.split_once(':') {
            None if setting.eq_ignore_ascii_case("bearer") => Some(Self::Bearer),
            Some(("header", name)) => Self::header(name),
            Some(("query", name)) if !name.is_empty() => Some(Self::Query(name.to_string())),
            _ => None,
        };
        place.ok_or_else(|| {
            anyhow::anyhow!(
                "token_in '{setting}' is not 'bearer', 'header:<name>', or 'query:<name>'"
            )
        })
    }

    /// A header place, unless the name cannot be a header or is one the
    /// HTTP client owns.
    pub(crate) fn header(name: &str) -> Option<Self> {
        let parsed = http::HeaderName::from_bytes(name.as_bytes()).ok()?;
        let owned = [
            http::header::HOST,
            http::header::CONTENT_LENGTH,
            http::header::CONTENT_TYPE,
            http::header::TRANSFER_ENCODING,
            http::header::CONNECTION,
        ];
        (!owned.contains(&parsed)).then(|| Self::Header(name.to_string()))
    }
}
