//! A short deterministic hash for shortening long ids.
//!
//! Ported from Pi `packages/ai/src/utils/hash.ts` (v1.1.0). It hashes UTF-16
//! code units, as the JavaScript original does, so ids match Pi's.

/// Pi's `shortHash`: two 32-bit lanes, each printed in base 36.
pub fn short_hash(text: &str) -> String {
    let mut h1: u32 = 0xdead_beef;
    let mut h2: u32 = 0x41c6_ce57;
    for unit in text.encode_utf16() {
        let unit = u32::from(unit);
        h1 = (h1 ^ unit).wrapping_mul(2_654_435_761);
        h2 = (h2 ^ unit).wrapping_mul(1_597_334_677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909);
    format!("{}{}", base36(h2), base36(h1))
}

fn base36(mut value: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_owned();
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Values computed with Pi's `shortHash` under Node 24.
    #[test]
    fn matches_pi_short_hash() {
        assert_eq!(short_hash(""), "k4n83c7h0j2b");
        assert_eq!(short_hash("call_abc|fc_123"), "jp01dsxufnom");
        assert_eq!(short_hash("🙈"), "kphsz0153ms3q");
    }
}
