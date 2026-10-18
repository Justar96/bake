//! The network-free rules of the public fetcher: URL validation, origin
//! comparison, content-type and charset handling, and the public-address
//! classification with its NAT64 translation check.
//!
//! Ported from `packages/web/web-fetch-http/src/policy.ts` and the address
//! half of `network.ts` (Bake 0.3), whose `isPublicIpAddress` is
//! `ipaddr.js` 2.5.0's `range() === 'unicast'`: the special ranges below
//! are that library's table. One rule is Bake's own and stricter: an IPv6
//! address outside the global unicast block `2000::/3`, such as the
//! deprecated IPv4-compatible form `::127.0.0.1`, is not public.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use reqwest::Url;

use super::{WEB_BLOCKED_URL, WEB_INVALID_URL, WEB_UNSUPPORTED_CONTENT_TYPE, WebError};

/// Maximum accepted request URL length (`WEB_FETCH_MAX_URL_LENGTH`).
pub const WEB_FETCH_MAX_URL_LENGTH: usize = 2048;

/// The body kinds the fetcher decodes (`FetchableKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchableKind {
    /// `text/html` and `application/xhtml+xml`.
    Html,
    /// Other text.
    Text,
}

/// `parseFetchUrl`: HTTP(S) only and no embedded credentials.
pub fn parse_fetch_url(input: &str) -> Result<Url, WebError> {
    let url = Url::parse(input)
        .map_err(|_| WebError::new(WEB_INVALID_URL, format!("invalid URL: {input}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(WebError::new(
            WEB_INVALID_URL,
            format!(
                "unsupported URL scheme \"{}:\" (only http and https are allowed)",
                url.scheme()
            ),
        ));
    }
    if !url.username().is_empty() || url.password().is_some_and(|password| !password.is_empty()) {
        return Err(WebError::new(
            WEB_BLOCKED_URL,
            "credentials in URLs are not allowed",
        ));
    }
    Ok(url)
}

/// `validateFetchUrl`: the length bound, then [`parse_fetch_url`]. The
/// bound counts UTF-16 code units, as the TypeScript's `length` did.
pub fn validate_fetch_url(input: &str) -> Result<Url, WebError> {
    if super::utf16_len(input) > WEB_FETCH_MAX_URL_LENGTH {
        return Err(WebError::new(
            WEB_INVALID_URL,
            format!("URL exceeds the maximum length of {WEB_FETCH_MAX_URL_LENGTH}"),
        ));
    }
    parse_fetch_url(input)
}

/// `isSameOrigin`: the same scheme, host, and port.
pub fn is_same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme() && a.host_str() == b.host_str() && a.port() == b.port()
}

/// `classifyContentType`: the decodable kind of a `Content-Type`, or
/// `None` for an unsupported (such as binary) or missing one.
pub fn classify_content_type(content_type: Option<&str>) -> Option<FetchableKind> {
    let raw = content_type.unwrap_or("");
    let mime = raw
        .split_once(';')
        .map_or(raw, |(mime, _)| mime)
        .trim()
        .to_ascii_lowercase();
    if mime == "text/html" || mime == "application/xhtml+xml" {
        return Some(FetchableKind::Html);
    }
    if mime.starts_with("text/")
        || mime == "application/json"
        || mime == "application/xml"
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
    {
        return Some(FetchableKind::Text);
    }
    None
}

/// `parseCharset`: the lower-cased `charset` parameter, if any.
pub fn parse_charset(content_type: Option<&str>) -> Option<String> {
    let value = content_type?;
    let lower = value.to_ascii_lowercase();
    let mut search = 0;
    while let Some(found) = lower[search..].find(';') {
        let start = search + found + 1;
        let rest = value[start..].trim_start();
        if rest.len() >= 7 && rest[..7].eq_ignore_ascii_case("charset") {
            let after = rest[7..].trim_start();
            if let Some(after) = after.strip_prefix('=') {
                let after = after.trim_start();
                let after = after.strip_prefix('"').unwrap_or(after);
                let end = after.find(['"', ';']).unwrap_or(after.len());
                let label = after[..end].trim();
                if !label.is_empty() {
                    return Some(label.to_ascii_lowercase());
                }
            }
        }
        search = start;
    }
    None
}

/// `decoderForCharset`: the WHATWG encoding for the declared label, UTF-8
/// when none is declared. A label no decoder recognizes, or one that maps
/// to the replacement encoding, fails rather than producing mojibake.
pub fn encoding_for_charset(
    charset: Option<&str>,
) -> Result<&'static encoding_rs::Encoding, WebError> {
    let Some(label) = charset else {
        return Ok(encoding_rs::UTF_8);
    };
    match encoding_rs::Encoding::for_label(label.as_bytes()) {
        Some(encoding) if encoding != encoding_rs::REPLACEMENT => Ok(encoding),
        _ => Err(WebError::new(
            WEB_UNSUPPORTED_CONTENT_TYPE,
            format!("unsupported charset \"{label}\""),
        )),
    }
}

/// `ipaddr.js`' IPv4 special ranges: every one is non-public.
const IPV4_SPECIAL: &[([u8; 4], u8)] = &[
    ([0, 0, 0, 0], 8),
    ([255, 255, 255, 255], 32),
    ([224, 0, 0, 0], 4),
    ([169, 254, 0, 0], 16),
    ([127, 0, 0, 0], 8),
    ([100, 64, 0, 0], 10),
    ([10, 0, 0, 0], 8),
    ([172, 16, 0, 0], 12),
    ([192, 168, 0, 0], 16),
    ([192, 0, 0, 0], 24),
    ([192, 0, 2, 0], 24),
    ([192, 88, 99, 0], 24),
    ([198, 18, 0, 0], 15),
    ([198, 51, 100, 0], 24),
    ([203, 0, 113, 0], 24),
    ([240, 0, 0, 0], 4),
    ([192, 175, 48, 0], 24),
    ([192, 31, 196, 0], 24),
    ([192, 52, 193, 0], 24),
];

/// `ipaddr.js`' IPv6 special ranges other than IPv4-mapped, which is
/// classified by its embedded address.
const IPV6_SPECIAL: &[([u16; 8], u8)] = &[
    ([0, 0, 0, 0, 0, 0, 0, 0], 128),
    ([0xfe80, 0, 0, 0, 0, 0, 0, 0], 10),
    ([0xff00, 0, 0, 0, 0, 0, 0, 0], 8),
    ([0, 0, 0, 0, 0, 0, 0, 1], 128),
    ([0xfc00, 0, 0, 0, 0, 0, 0, 0], 7),
    ([0xfec0, 0, 0, 0, 0, 0, 0, 0], 10),
    ([0x100, 0, 0, 0, 0, 0, 0, 0], 64),
    ([0, 0, 0, 0, 0xffff, 0, 0, 0], 96),
    ([0x64, 0xff9b, 0, 0, 0, 0, 0, 0], 96),
    ([0x64, 0xff9b, 0x1, 0, 0, 0, 0, 0], 48),
    ([0x2002, 0, 0, 0, 0, 0, 0, 0], 16),
    ([0x2001, 0, 0, 0, 0, 0, 0, 0], 32),
    ([0x2001, 0x2, 0, 0, 0, 0, 0, 0], 48),
    ([0x2001, 0x3, 0, 0, 0, 0, 0, 0], 32),
    ([0x2001, 0x4, 0x112, 0, 0, 0, 0, 0], 48),
    ([0x2620, 0x4f, 0x8000, 0, 0, 0, 0, 0], 48),
    ([0x2001, 0x10, 0, 0, 0, 0, 0, 0], 28),
    ([0x2001, 0x20, 0, 0, 0, 0, 0, 0], 28),
    ([0x2001, 0x30, 0, 0, 0, 0, 0, 0], 28),
    ([0x5f00, 0, 0, 0, 0, 0, 0, 0], 16),
    ([0x2001, 0, 0, 0, 0, 0, 0, 0], 23),
    ([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0], 32),
    ([0x3fff, 0, 0, 0, 0, 0, 0, 0], 20),
];

fn prefix_match(address: &[u8], network: &[u8], bits: u8) -> bool {
    let bits = usize::from(bits);
    let whole = bits / 8;
    if address.get(..whole) != network.get(..whole) {
        return false;
    }
    let rest = bits % 8;
    if rest == 0 {
        return true;
    }
    let mask = 0xffu8 << (8 - rest);
    match (address.get(whole), network.get(whole)) {
        (Some(a), Some(n)) => a & mask == n & mask,
        _ => false,
    }
}

fn ipv4_is_public(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    !IPV4_SPECIAL
        .iter()
        .any(|(network, bits)| prefix_match(&octets, network, *bits))
}

fn ipv6_is_public(address: Ipv6Addr) -> bool {
    let octets = address.octets();
    // Bake's own: only the global unicast block can be public.
    if octets[0] & 0xe0 != 0x20 {
        return false;
    }
    !IPV6_SPECIAL
        .iter()
        .any(|(network, bits)| prefix_match(&octets, &Ipv6Addr::from(*network).octets(), *bits))
}

/// The IPv4 address an IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) carries.
fn ipv4_mapped(address: Ipv6Addr) -> Option<Ipv4Addr> {
    let octets = address.octets();
    (octets[..10] == [0; 10] && octets[10] == 0xff && octets[11] == 0xff)
        .then(|| Ipv4Addr::new(octets[12], octets[13], octets[14], octets[15]))
}

/// `isPublicIpAddress`: whether `address` is a globally reachable unicast
/// destination. IPv4-mapped IPv6 is classified by its embedded IPv4;
/// transition and translation prefixes stay blocked, since their eventual
/// IPv4 destination cannot be pinned here.
pub fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => ipv4_is_public(v4),
        IpAddr::V6(v6) => match ipv4_mapped(v6) {
            Some(v4) => ipv4_is_public(v4),
            None => ipv6_is_public(v6),
        },
    }
}

/// `isPublicIpAddress` on text: `false` for anything that is not an IP
/// address, bracketed IPv6 included.
pub fn is_public_ip_text(input: &str) -> bool {
    strip_ipv6_brackets(input)
        .parse::<IpAddr>()
        .is_ok_and(is_public_ip)
}

/// WHATWG URLs keep the brackets around an IPv6 host; address parsers do
/// not take them.
pub(crate) fn strip_ipv6_brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host)
}

/// RFC 6052 prefix lengths that may carry an IPv4 destination through
/// NAT64.
const RFC6052_PREFIX_LENGTHS: [u8; 6] = [32, 40, 48, 56, 64, 96];
/// RFC 7050's discovery name and the IPv4 sentinels it resolves to.
pub(crate) const IPV4ONLY_DISCOVERY_HOST: &str = "ipv4only.arpa";
const IPV4ONLY_SENTINELS: [Ipv4Addr; 2] =
    [Ipv4Addr::new(192, 0, 0, 170), Ipv4Addr::new(192, 0, 0, 171)];

/// One active NAT64 prefix (`Nat64Prefix`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nat64Prefix {
    bytes: Vec<u8>,
    length: u8,
}

/// `embeddedIpv4Address`: the IPv4 address of one RFC 6052 layout.
fn embedded_ipv4(bytes: &[u8; 16], prefix_length: u8) -> Option<Ipv4Addr> {
    if prefix_length == 96 {
        return Some(Ipv4Addr::new(bytes[12], bytes[13], bytes[14], bytes[15]));
    }
    if bytes[8] != 0 {
        return None;
    }
    let prefix_bytes = usize::from(prefix_length / 8);
    let before = 8usize.checked_sub(prefix_bytes)?;
    let mut ipv4: Vec<u8> = bytes.get(prefix_bytes..prefix_bytes + before)?.to_vec();
    ipv4.extend_from_slice(bytes.get(9..9 + 4 - before)?);
    let octets: [u8; 4] = ipv4.try_into().ok()?;
    Some(Ipv4Addr::from(octets))
}

/// `discoverNat64Prefixes`: the prefixes the answers for
/// [`IPV4ONLY_DISCOVERY_HOST`] reveal.
pub fn nat64_prefixes(discovered: &[IpAddr]) -> Vec<Nat64Prefix> {
    let mut prefixes: Vec<Nat64Prefix> = Vec::new();
    for entry in discovered {
        let IpAddr::V6(v6) = entry else {
            continue;
        };
        let bytes = v6.octets();
        for length in RFC6052_PREFIX_LENGTHS {
            let Some(embedded) = embedded_ipv4(&bytes, length) else {
                continue;
            };
            if !IPV4ONLY_SENTINELS.contains(&embedded) {
                continue;
            }
            let prefix = Nat64Prefix {
                bytes: bytes[..usize::from(length / 8)].to_vec(),
                length,
            };
            if !prefixes.contains(&prefix) {
                prefixes.push(prefix);
            }
        }
    }
    prefixes
}

/// `translatedIpv4Address`: the IPv4 address an IPv6 answer reaches
/// through one of `prefixes`.
pub fn translated_ipv4(address: IpAddr, prefixes: &[Nat64Prefix]) -> Option<Ipv4Addr> {
    let IpAddr::V6(v6) = address else {
        return None;
    };
    let bytes = v6.octets();
    prefixes
        .iter()
        .filter(|prefix| bytes.starts_with(&prefix.bytes))
        .find_map(|prefix| embedded_ipv4(&bytes, prefix.length))
}

#[cfg(test)]
mod tests {
    use super::*;

    // fetch-http.spec.ts: "policy helpers" > "validates scheme,
    // credentials, and length".
    #[test]
    fn validates_scheme_credentials_and_length() {
        assert_eq!(
            parse_fetch_url("https://example.com/preflight")
                .expect("valid")
                .path(),
            "/preflight"
        );
        assert_eq!(
            validate_fetch_url("https://example.com/x")
                .expect("valid")
                .host_str(),
            Some("example.com")
        );
        let code = |input: &str| validate_fetch_url(input).map_err(|error| error.code);
        assert_eq!(code("ftp://example.com").err(), Some(WEB_INVALID_URL));
        assert_eq!(code("not a url").err(), Some(WEB_INVALID_URL));
        assert_eq!(
            code("https://user:pass@example.com").err(),
            Some(WEB_BLOCKED_URL)
        );
        let prefix = "https://example.com/";
        let exact = format!(
            "{prefix}{}",
            "a".repeat(WEB_FETCH_MAX_URL_LENGTH - prefix.len())
        );
        assert_eq!(validate_fetch_url(&exact).expect("exact").as_str(), exact);
        assert_eq!(code(&format!("{exact}a")).err(), Some(WEB_INVALID_URL));
        assert_eq!(
            validate_fetch_url("ftp://example.com")
                .expect_err("ftp")
                .message,
            "unsupported URL scheme \"ftp:\" (only http and https are allowed)"
        );
        assert_eq!(
            validate_fetch_url("not a url")
                .expect_err("invalid")
                .message,
            "invalid URL: not a url"
        );
    }

    // "classifies content types".
    #[test]
    fn classifies_content_types() {
        assert_eq!(
            classify_content_type(Some("text/html; charset=utf-8")),
            Some(FetchableKind::Html)
        );
        assert_eq!(
            classify_content_type(Some("application/xhtml+xml")),
            Some(FetchableKind::Html)
        );
        assert_eq!(
            classify_content_type(Some("text/plain")),
            Some(FetchableKind::Text)
        );
        assert_eq!(
            classify_content_type(Some("application/json")),
            Some(FetchableKind::Text)
        );
        assert_eq!(
            classify_content_type(Some("application/ld+json")),
            Some(FetchableKind::Text)
        );
        assert_eq!(classify_content_type(Some("image/png")), None);
        assert_eq!(classify_content_type(None), None);
    }

    // "compares origins".
    #[test]
    fn compares_origins() {
        let url = |text: &str| Url::parse(text).expect("url");
        assert!(is_same_origin(
            &url("https://a.com/x"),
            &url("https://a.com/y")
        ));
        assert!(!is_same_origin(
            &url("https://a.com"),
            &url("https://b.com")
        ));
        assert!(!is_same_origin(&url("http://a.com"), &url("https://a.com")));
        assert!(is_same_origin(
            &url("http://a.com:80/"),
            &url("http://a.com/")
        ));
    }

    // "parses the charset parameter".
    #[test]
    fn parses_the_charset_parameter() {
        assert_eq!(
            parse_charset(Some("text/html; charset=UTF-8")).as_deref(),
            Some("utf-8")
        );
        assert_eq!(
            parse_charset(Some("text/plain; charset=\"iso-8859-1\"")).as_deref(),
            Some("iso-8859-1")
        );
        assert_eq!(parse_charset(Some("text/plain")), None);
        assert_eq!(parse_charset(None), None);
    }

    // "builds a decoder for a charset and defaults to UTF-8".
    #[test]
    fn builds_a_decoder_for_a_charset_and_defaults_to_utf8() {
        assert_eq!(encoding_for_charset(None).expect("utf-8").name(), "UTF-8");
        assert_eq!(
            encoding_for_charset(Some("iso-8859-1"))
                .expect("latin-1")
                .name(),
            "windows-1252"
        );
        assert_eq!(
            encoding_for_charset(Some("not-a-charset"))
                .expect_err("unknown")
                .code,
            WEB_UNSUPPORTED_CONTENT_TYPE
        );
        assert!(encoding_for_charset(Some("iso-2022-kr")).is_err());
    }

    // "public-network policy" > "accepts only globally reachable unicast
    // addresses", with Bake's stricter IPv6 rule.
    #[test]
    fn accepts_only_globally_reachable_unicast_addresses() {
        for address in ["8.8.8.8", "2001:4860:4860::8888", "::ffff:8.8.8.8"] {
            assert!(is_public_ip_text(address), "{address}");
        }
        for address in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.168.1.1",
            "192.0.2.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fe80::1",
            "fc00::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "64:ff9b::808:808",
            "not-an-ip",
            // Bake's own: outside 2000::/3.
            "::127.0.0.1",
            "100::1",
        ] {
            assert!(!is_public_ip_text(address), "{address}");
        }
        assert!(is_public_ip_text("[2001:4860:4860::8888]"));
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("ip")
    }

    // "rejects a network-specific NAT64 address that translates to private
    // IPv4" and "accepts ... public IPv4".
    #[test]
    fn translates_through_a_discovered_network_specific_prefix() {
        let prefixes = nat64_prefixes(&[ip("2001:4860:64:64::c000:aa")]);
        assert_eq!(
            translated_ipv4(ip("2001:4860:64:64::7f00:1"), &prefixes),
            Some(Ipv4Addr::new(127, 0, 0, 1))
        );
        assert_eq!(
            translated_ipv4(ip("2001:4860:64:64::808:808"), &prefixes),
            Some(Ipv4Addr::new(8, 8, 8, 8))
        );
    }

    // "deduplicates discovered prefixes and ignores addresses outside their
    // translation layout".
    #[test]
    fn deduplicates_prefixes_and_ignores_addresses_outside_their_layout() {
        let prefixes = nat64_prefixes(&[
            ip("2001:4860:64:64::c000:aa"),
            ip("2001:4860:64:64::c000:ab"),
            ip("2001:4860:64:64:c0:0:aa00:0"),
            ip("192.0.0.170"),
        ]);
        assert_eq!(
            prefixes,
            [
                Nat64Prefix {
                    bytes: vec![0x20, 0x01, 0x48, 0x60, 0, 0x64, 0, 0x64, 0, 0, 0, 0],
                    length: 96
                },
                Nat64Prefix {
                    bytes: vec![0x20, 0x01, 0x48, 0x60, 0, 0x64, 0, 0x64],
                    length: 64
                },
            ]
        );
        assert_eq!(
            translated_ipv4(ip("2001:4860:65:64::808:808"), &prefixes),
            None
        );
        assert_eq!(
            translated_ipv4(ip("2001:4860:64:64:100::1"), &prefixes),
            None
        );
    }
}
