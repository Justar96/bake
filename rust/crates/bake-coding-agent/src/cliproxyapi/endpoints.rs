//! The proxy's URL forms: `cliProxyEndpoints` in
//! `apps/tui/packages/app/src/cliproxyapi.ts`.
//!
//! The TypeScript parses with the WHATWG `URL` class; the `url` crate that
//! reqwest re-exports implements the same standard, so hosts, ports,
//! percent-encoding, and origins serialize alike.

use std::fmt;

use reqwest::Url;

/// The three addresses one proxy answers on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliProxyEndpoints {
    /// The proxy root, without `/v1` or `/backend-api`. Anthropic Messages
    /// models are sent here, because that protocol joins `/v1/messages`.
    pub root: String,
    /// `<root>/v1/models?client_version=pi`, the catalog.
    pub models: String,
    /// `<root>/v1`, the route's base URL.
    pub inference: String,
}

/// Why an address is not a proxy URL. The messages are the TypeScript's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliProxyUrlError {
    /// Empty after trimming.
    Empty,
    /// Not a parseable URL (`new URL` throws `Invalid URL`).
    Invalid,
    /// Not HTTP(S), or carrying credentials, a query, or a fragment.
    Disallowed,
}

impl fmt::Display for CliProxyUrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "CLIProxyAPI URL is empty",
            Self::Invalid => "Invalid URL",
            Self::Disallowed => {
                "CLIProxyAPI URL must be an HTTP(S) address without credentials, query, or fragment"
            }
        })
    }
}

impl std::error::Error for CliProxyUrlError {}

/// JavaScript's `String.prototype.trim`: its WhiteSpace and LineTerminator
/// sets, which include U+FEFF and exclude U+0085, unlike [`str::trim`].
pub(crate) fn js_trim(value: &str) -> &str {
    value.trim_matches(|c: char| {
        matches!(
            c,
            '\u{9}'
                | '\u{A}'
                | '\u{B}'
                | '\u{C}'
                | '\u{D}'
                | ' '
                | '\u{A0}'
                | '\u{1680}'
                | '\u{2000}'
                ..='\u{200A}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202F}'
                    | '\u{205F}'
                    | '\u{3000}'
                    | '\u{FEFF}'
        )
    })
}

/// `cliProxyEndpoints`: accept a root URL, a `/v1` URL, or the proxy's
/// native `/backend-api` URL, with or without a scheme (`http://` is
/// assumed).
pub fn cli_proxy_endpoints(input: &str) -> Result<CliProxyEndpoints, CliProxyUrlError> {
    let raw = js_trim(input);
    if raw.is_empty() {
        return Err(CliProxyUrlError::Empty);
    }
    let has_scheme = starts_with_ignore_ascii_case(raw, "http://")
        || starts_with_ignore_ascii_case(raw, "https://");
    let text = if has_scheme {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    };
    let url = Url::parse(&text).map_err(|_| CliProxyUrlError::Invalid)?;
    // WHATWG `search` and `hash` are empty for a bare `?` or `#`, so only a
    // non-empty query or fragment is refused, as the TypeScript refuses it.
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some_and(|password| !password.is_empty())
        || url.query().is_some_and(|query| !query.is_empty())
        || url.fragment().is_some_and(|fragment| !fragment.is_empty())
    {
        return Err(CliProxyUrlError::Disallowed);
    }
    let trimmed = url.path().trim_end_matches('/');
    let path = trimmed
        .strip_suffix("/v1")
        .or_else(|| trimmed.strip_suffix("/backend-api"))
        .unwrap_or(trimmed);
    let root = format!("{}{path}", url.origin().ascii_serialization());
    Ok(CliProxyEndpoints {
        models: format!("{root}/v1/models?client_version=pi"),
        inference: format!("{root}/v1"),
        root,
    })
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    // cliproxyapi.test.ts: "CLIProxyAPI endpoints" > "accepts the published
    // root, /v1, and /backend-api forms".
    #[test]
    fn accepts_the_published_root_v1_and_backend_api_forms() {
        for input in [
            "127.0.0.1:8317",
            "http://127.0.0.1:8317/v1",
            "http://127.0.0.1:8317/backend-api",
        ] {
            assert_eq!(
                cli_proxy_endpoints(input),
                Ok(CliProxyEndpoints {
                    root: "http://127.0.0.1:8317".into(),
                    models: "http://127.0.0.1:8317/v1/models?client_version=pi".into(),
                    inference: "http://127.0.0.1:8317/v1".into(),
                })
            );
        }
        assert_eq!(
            cli_proxy_endpoints("https://proxy.example/prefix/v1")
                .map(|endpoints| endpoints.inference),
            Ok("https://proxy.example/prefix/v1".to_owned())
        );
    }

    // cliproxyapi.test.ts: "CLIProxyAPI endpoints" > "rejects a URL carrying
    // other request data".
    #[test]
    fn rejects_a_url_carrying_other_request_data() {
        assert_eq!(
            cli_proxy_endpoints("https://user:pass@proxy.example"),
            Err(CliProxyUrlError::Disallowed)
        );
        assert_eq!(
            cli_proxy_endpoints("https://proxy.example?key=secret"),
            Err(CliProxyUrlError::Disallowed)
        );
    }

    // cliProxyEndpoints' own branches: the empty and unparseable errors and
    // their messages, a scheme in any case, trailing slashes, one suffix
    // stripped, and a bare `?` or `#` (empty WHATWG search and hash). Every
    // expected value is the TypeScript's own output for the same input.
    #[test]
    fn follows_the_typescript_branches() {
        assert_eq!(
            cli_proxy_endpoints(" \t\u{FEFF}"),
            Err(CliProxyUrlError::Empty)
        );
        assert_eq!(
            CliProxyUrlError::Empty.to_string(),
            "CLIProxyAPI URL is empty"
        );
        assert_eq!(
            cli_proxy_endpoints("bad url with spaces"),
            Err(CliProxyUrlError::Invalid)
        );
        assert_eq!(
            CliProxyUrlError::Disallowed.to_string(),
            "CLIProxyAPI URL must be an HTTP(S) address without credentials, query, or fragment"
        );
        let root = |input: &str| cli_proxy_endpoints(input).map(|endpoints| endpoints.root);
        assert_eq!(
            root("HTTPS://Proxy.Example:443/"),
            Ok("https://proxy.example".into())
        );
        assert_eq!(root("http://h:80/a/v1///"), Ok("http://h/a".into()));
        assert_eq!(root("http://h/v1/v1"), Ok("http://h/v1".into()));
        assert_eq!(root("http://h/backend-api/"), Ok("http://h".into()));
        assert_eq!(root("http://h/v10"), Ok("http://h/v10".into()));
        assert_eq!(root("http://h/?"), Ok("http://h".into()));
        assert_eq!(root("http://h/#"), Ok("http://h".into()));
        assert_eq!(root("http://[::1]:8317/v1"), Ok("http://[::1]:8317".into()));
        assert_eq!(root("http://h#frag"), Err(CliProxyUrlError::Disallowed));
        assert_eq!(root("http://user@h"), Err(CliProxyUrlError::Disallowed));
        assert_eq!(root("http://:@h"), Ok("http://h".into()));
        assert_eq!(root("ftp://x"), Ok("http://ftp//x".into()));
    }
}
