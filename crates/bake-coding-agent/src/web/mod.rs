//! Web access for the `web_fetch` and `web_search` tools
//! ([`crate::tools::web_tools`]): an anonymous public HTTP(S) fetcher and a
//! search that runs through the CLIProxyAPI route.
//!
//! Pi v1.3.0 (`71023aec`; MIT, see the crate `NOTICE`) has no web tools.
//! Ported from Bake 0.3's TypeScript and its tests:
//!
//! | Module | TypeScript source |
//! |---|---|
//! | this module ([`WebError`] and its codes) | `packages/web/web/src/{index,types}.ts` |
//! | [`policy`] | `packages/web/web-fetch-http/src/{policy,network}.ts` (URL, content-type, charset, and public-address rules) |
//! | [`fetch`](mod@fetch) | `packages/web/web-fetch-http/src/{provider,network,index}.ts` |
//! | [`html`] | the HTML conversion of `packages/web/tool-web/src/fetch.ts` |
//! | [`search`] | `packages/web/web-search-deepseek/src/{provider,types}.ts` |
//!
//! `docs/subsystems/web.md` describes the 0.3 design. 0.3 selected one
//! provider per capability through the `ctx.web` registry; Bake has
//! exactly one of each, so the registry and its selection codes
//! (`WEB_PROVIDER_AMBIGUOUS` and the like) are not ported.
//!
//! # Search through CLIProxyAPI
//!
//! 0.3 searched with DeepSeek's Anthropic-compatible endpoint. By the
//! owner's decision, the Rust search keeps that request's structure (an
//! Anthropic Messages request carrying the server-side
//! `web_search_20250305` tool, results taken only from structured
//! `web_search_tool_result` blocks) and sends it through the session's
//! CLIProxyAPI route instead, with one of the route's Anthropic Messages
//! models ([`search::select_search_model`]).

pub mod fetch;
pub mod html;
pub mod policy;
pub mod search;
#[cfg(test)]
pub(crate) mod test_server;

use std::fmt;

/// A failure to search or to retrieve and represent a resource
/// (`WebError`). The message is what the model reads; the code is for
/// callers that branch on the cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebError {
    /// One of the `WEB_*` codes.
    pub code: &'static str,
    /// The failure, in 0.3's words.
    pub message: String,
}

impl WebError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for WebError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WebError {}

/// The caller cancelled.
pub const WEB_ABORTED: &str = "WEB_ABORTED";
/// The provider failed, including a transport failure.
pub const WEB_PROVIDER_ERROR: &str = "WEB_PROVIDER_ERROR";
/// The search route has no usable model or key.
pub const WEB_PROVIDER_UNAVAILABLE: &str = "WEB_PROVIDER_UNAVAILABLE";
/// The URL does not parse or is not HTTP(S).
pub const WEB_INVALID_URL: &str = "WEB_INVALID_URL";
/// The URL carries credentials or names a non-public destination.
pub const WEB_BLOCKED_URL: &str = "WEB_BLOCKED_URL";
/// A redirect that is not followed.
pub const WEB_REDIRECT_BLOCKED: &str = "WEB_REDIRECT_BLOCKED";
/// A declared body larger than the byte cap.
pub const WEB_FETCH_TOO_LARGE: &str = "WEB_FETCH_TOO_LARGE";
/// The fetch's own time bound.
pub const WEB_FETCH_TIMEOUT: &str = "WEB_FETCH_TIMEOUT";
/// A body that is not text, or a charset that cannot be decoded.
pub const WEB_UNSUPPORTED_CONTENT_TYPE: &str = "WEB_UNSUPPORTED_CONTENT_TYPE";

/// An error's sources joined, without the client's own line, which names
/// the URL with any query it carries.
pub(crate) fn source_chain(error: &dyn std::error::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut source = error.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !parts.iter().any(|part| part.contains(&text)) {
            parts.push(text);
        }
        source = cause.source();
    }
    if parts.is_empty() {
        error.to_string()
    } else {
        parts.join(": ")
    }
}

/// `value` cut to at most `max` UTF-16 code units, the unit JavaScript's
/// `length` and `slice` count, never inside a character; and whether it
/// was cut.
pub(crate) fn truncate_utf16(value: &str, max: usize) -> (&str, bool) {
    let mut units = 0usize;
    for (index, c) in value.char_indices() {
        units += c.len_utf16();
        if units > max {
            return (&value[..index], true);
        }
    }
    (value, false)
}

/// `value`'s length in UTF-16 code units.
pub(crate) fn utf16_len(value: &str) -> usize {
    value.chars().map(char::len_utf16).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_by_utf16_units_at_character_boundaries() {
        assert_eq!(truncate_utf16("abc", 3), ("abc", false));
        assert_eq!(truncate_utf16("abcd", 3), ("abc", true));
        // An astral character is two units and is never split.
        assert_eq!(truncate_utf16("a😀b", 2), ("a", true));
        assert_eq!(truncate_utf16("a😀b", 3), ("a😀", true));
        assert_eq!(utf16_len("a😀"), 3);
    }
}
