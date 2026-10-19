//! The script environment: Pi's codemode prelude, evaluated in each VM before
//! the script. The JavaScript is `assets/prelude.js`, copied verbatim from Pi
//! v1.3.0's `packages/codemode/src/runtime/prelude-source.ts` with its notice
//! (MIT, see `NOTICE`).
//!
//! The prelude keeps the host bridge in a closure and builds `tools`,
//! `ALL_TOOLS`, `text`, `image`, `exit`, `console`, `store`, `load`, and the
//! declared globals on top of it, after freezing every built-in. It evaluates
//! to `(bridge, toolsJson, globalsJson, storeJson) => { settle, run, stalled }`;
//! `vm.rs` drives that contract.

use std::sync::OnceLock;

/// The asset, notice included.
const ASSET: &str = include_str!("../assets/prelude.js");
/// The line that ends the notice; the prelude is the text after it.
const START_MARKER: &str = "// @prelude-start\n";

/// Characters of JSON one `store()` value may hold.
pub const MAX_STORE_VALUE_CHARS: usize = 256 * 1024;
/// Characters of keys and JSON all stored values may hold together.
pub const MAX_STORE_TOTAL_CHARS: usize = 1024 * 1024;
/// Characters of text and base64 image data one script may output.
pub const MAX_OUTPUT_CHARS: usize = 16 * 1024 * 1024;
/// `text()`, `image()`, and `console.*` calls one script may make.
pub const MAX_OUTPUT_ITEMS: usize = 100_000;

/// The prelude's JavaScript, exactly Pi's `PRELUDE_SOURCE`.
pub(crate) fn source() -> &'static str {
    static SOURCE: OnceLock<&'static str> = OnceLock::new();
    SOURCE.get_or_init(|| {
        let body = ASSET
            .split_once(START_MARKER)
            .map_or(ASSET, |(_, body)| body);
        body.strip_suffix('\n').unwrap_or(body)
    })
}

/// FNV-1a over the prelude's UTF-16 code units, as 8 hex digits: identifies the
/// prelude in the deterministic fingerprint, as Pi's `vm.ts` computes it.
pub(crate) fn hash() -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for unit in source().encode_utf16() {
        hash ^= u32::from(unit);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_pi_v1_3_0s_prelude_byte_for_byte() {
        // Pi v1.3.0's `fnv1a(PRELUDE_SOURCE)` and the source's length, computed
        // from `prelude-source.ts` itself.
        assert_eq!(hash(), "8c6c9242");
        assert_eq!(source().len(), 17_233);
        assert!(source().starts_with("(function (bridge, toolsJson, globalsJson, storeJson) {"));
        assert!(source().ends_with("})"));
        assert!(source().contains(&format!("json.length > {MAX_STORE_VALUE_CHARS}")));
        assert!(source().contains(&format!("next > {MAX_STORE_TOTAL_CHARS}")));
        assert!(source().contains(&format!(
            "outputChars > {MAX_OUTPUT_CHARS} || outputItems > {MAX_OUTPUT_ITEMS}"
        )));
    }
}
