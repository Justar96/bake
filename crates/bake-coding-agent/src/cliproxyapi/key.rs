//! The CLIProxyAPI key a sign-in checks and sends: trimmed, non-empty,
//! printable ASCII, so any HTTP header can carry it. No `Debug` or
//! `Display` output holds it.

use std::fmt;

/// An API key. Its `Debug` and `Display` never show it.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// The key, for the request that sends it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

impl fmt::Display for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Why a key cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRejection {
    /// Blank after trimming.
    Empty,
    /// Holds a character outside printable ASCII.
    IllegalCharacters,
}

/// Trims `raw`, then requires printable ASCII without spaces.
pub fn normalize_api_key(raw: &str) -> Result<ApiKey, KeyRejection> {
    let value = super::endpoints::js_trim(raw);
    if value.is_empty() {
        return Err(KeyRejection::Empty);
    }
    if !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err(KeyRejection::IllegalCharacters);
    }
    Ok(ApiKey(value.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_trimmed_printable_ascii_and_redacted() {
        let key = normalize_api_key("  sk-fake-1 \n").expect("key");
        assert_eq!(key.expose(), "sk-fake-1");
        assert_eq!(format!("{key:?} {key}"), "ApiKey(<redacted>) <redacted>");
        assert_eq!(normalize_api_key(" \t "), Err(KeyRejection::Empty));
        assert_eq!(
            normalize_api_key("sk fake"),
            Err(KeyRejection::IllegalCharacters)
        );
        assert_eq!(
            normalize_api_key("sk-é"),
            Err(KeyRejection::IllegalCharacters)
        );
    }
}
