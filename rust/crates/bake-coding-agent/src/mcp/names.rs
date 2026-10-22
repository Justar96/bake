//! The model-facing names of MCP tools, a retained Bake contract.
//!
//! Ported from `publicToolName` in Bake's
//! `packages/mcp/mcp-client/src/tools.ts`: every MCP tool has the identity
//! `(server, tool)` and the public name `mcp__<server>__<tool>`. When a
//! character outside `[A-Za-z0-9_-]` is replaced or the name is cut to 64
//! characters, the first 12 hex digits of the identity's SHA-256 are
//! appended to distinguish normalized identities. Publication refuses a
//! remaining name collision. The public name is never
//! parsed back: calls send the tool's own name. Pi's `createMcpToolName`
//! also maps `-` and hashes only on length or a taken name; Bake keeps its
//! own algorithm as required by the Pi-first scope plan.

use ring::digest::{Context, SHA256};

/// The longest public name, the provider function-name limit.
pub const MAX_PUBLIC_NAME_LENGTH: usize = 64;

/// Hex digits of the identity hash.
const HASH_LENGTH: usize = 12;

/// The prefix every MCP tool name starts with.
pub const MCP_TOOL_PREFIX: &str = "mcp__";

/// `text` with each UTF-16 unit outside `[A-Za-z0-9_-]` replaced by `_`.
fn normalize(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
            normalized.push(character);
        } else {
            // The TypeScript regex replaces each UTF-16 code unit.
            for _ in 0..character.len_utf16() {
                normalized.push('_');
            }
        }
    }
    normalized
}

/// The namespace of `server`'s tools for the `codemode` tool,
/// `mcp__<server>`, never hashed: the prefix its tools' unhashed names
/// share. Bake keeps `-` (`mcp__my-server`), where Pi's `mcpNamespace` maps
/// it to `_` (`mcp__my_server`).
pub fn mcp_namespace(server: &str) -> String {
    normalize(&format!("{MCP_TOOL_PREFIX}{server}"))
}

/// The public name of `tool` on `server`.
pub fn public_tool_name(server: &str, tool: &str) -> String {
    let joined = format!("{MCP_TOOL_PREFIX}{server}__{tool}");
    let normalized = normalize(&joined);
    if normalized == joined && normalized.len() <= MAX_PUBLIC_NAME_LENGTH {
        return normalized;
    }
    let mut context = Context::new(&SHA256);
    context.update(server.as_bytes());
    context.update(&[0]);
    context.update(tool.as_bytes());
    let digest = context.finish();
    let hash: String = digest
        .as_ref()
        .iter()
        .take(HASH_LENGTH / 2)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    // ASCII only, so a byte index is a character index.
    let keep = normalized
        .len()
        .min(MAX_PUBLIC_NAME_LENGTH - HASH_LENGTH - 1);
    format!("{}_{hash}", &normalized[..keep])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names computed by the TypeScript `publicToolName`
    /// (`tests/fixtures/mcp/public-names.mjs` reproduces it with Node's
    /// `crypto`; `packages/mcp/mcp-client/tests/mcp-client.spec.ts` pins its
    /// shapes).
    const RECORDED: &str = include_str!("../../tests/fixtures/mcp/public-names.json");

    #[test]
    fn names_match_the_typescript_algorithm() {
        let cases: Vec<(String, String, String)> = serde_json::from_str(RECORDED).unwrap();
        assert!(cases.len() >= 8);
        for (server, tool, expected) in cases {
            assert_eq!(
                public_tool_name(&server, &tool),
                expected,
                "{server} {tool}"
            );
        }
    }

    // `mcp-client.spec.ts`, "publicToolName".
    #[test]
    fn clean_names_join_verbatim_and_others_are_hashed() {
        assert_eq!(
            public_tool_name("github", "create_issue"),
            "mcp__github__create_issue"
        );
        assert_eq!(
            public_tool_name("everything", "get-sum"),
            "mcp__everything__get-sum"
        );
        let replaced = public_tool_name("srv", "admin.reset");
        assert!(replaced.starts_with("mcp__srv__admin_reset_"), "{replaced}");
        assert_eq!(replaced.len(), "mcp__srv__admin_reset_".len() + 12);
        let long = public_tool_name("srv", &"a".repeat(80));
        assert_eq!(long.len(), 64);
        assert!(long.starts_with("mcp__srv__aaa"));
        assert_ne!(
            public_tool_name("srv", "admin.reset"),
            public_tool_name("srv", "admin_reset")
        );
        // An astral character is two UTF-16 units, so two underscores.
        assert!(public_tool_name("s", "a😀").starts_with("mcp__s__a___"));
    }

    /// A namespace keeps the server's `-`, where Pi's is `mcp__my_server`.
    #[test]
    fn namespaces_keep_the_server_name() {
        assert_eq!(mcp_namespace("my-server"), "mcp__my-server");
        assert!(public_tool_name("my-server", "get").starts_with(&mcp_namespace("my-server")));
    }
}
