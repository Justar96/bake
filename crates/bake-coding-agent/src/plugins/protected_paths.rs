//! The `protected-paths` guard, ported from Pi v1.3.0
//! `examples/extensions/protected-paths.ts`
//! (`71023aec1546d6d677a65669c5665c0927ce0dd0`, MIT): it blocks `write` and
//! `edit` calls whose `path` contains a protected fragment. Pi's UI notice
//! is not ported; the session records the block instead.
//!
//! This is a convenience, not a confidentiality boundary: `bash` and `read`
//! still reach those paths, and a fragment match does not resolve links.
//! D24's permission checks remain the enforcement floor.
//!
//! Matching is Pi's: a case-sensitive substring test on the `path` argument
//! as the model wrote it, with no normalization. So `.ENV` on a
//! case-insensitive file system (macOS and Windows by default), and
//! `repo\.git\config` or `node_modules\x` with Windows separators, are not
//! blocked; nor is a path that reaches a protected file through `..` or a
//! link without containing the fragment. A fragment also matches inside a
//! longer name (`.env` blocks `app/.envrc`).

use bake_agent::BoxFuture;

use super::{Plugin, PluginManifest, ToolCallVeto, ToolCallView, truncate};

/// The longest path quoted in a block reason, in bytes.
const MAX_QUOTED_PATH: usize = 1024;

/// Blocks `write` and `edit` to paths containing any of its fragments.
#[derive(Debug, Clone)]
pub struct ProtectedPaths {
    fragments: Vec<String>,
}

impl ProtectedPaths {
    /// The plugin id.
    pub const ID: &'static str = "protected-paths";

    /// A guard over `fragments`, matched as substrings of the call's
    /// `path`, as Pi's `path.includes(p)`.
    pub fn new<I, S>(fragments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            fragments: fragments
                .into_iter()
                .map(Into::into)
                .filter(|fragment: &String| !fragment.is_empty())
                .collect(),
        }
    }

    /// Pi's example list: `.env`, `.git/`, and `node_modules/`.
    pub fn pi_default() -> Self {
        Self::new([".env", ".git/", "node_modules/"])
    }
}

impl Plugin for ProtectedPaths {
    fn manifest(&self) -> PluginManifest {
        // The id, version, and tools are constants that satisfy the rules.
        PluginManifest::new(Self::ID, env!("CARGO_PKG_VERSION"), ["edit", "write"])
            .unwrap_or_else(|error| unreachable!("protected-paths manifest: {error}"))
    }

    fn on_tool_call<'a>(
        &'a self,
        call: ToolCallView<'a>,
    ) -> BoxFuture<'a, Result<ToolCallVeto, String>> {
        Box::pin(async move {
            if call.tool_name != "write" && call.tool_name != "edit" {
                return Ok(ToolCallVeto::Continue);
            }
            // Both tools require a string `path`, validated before this
            // runs; a call without one is refused rather than allowed.
            let Some(path) = call.args.get("path").and_then(serde_json::Value::as_str) else {
                return Err(format!("{} call has no string path", call.tool_name));
            };
            if self
                .fragments
                .iter()
                .any(|fragment| path.contains(fragment.as_str()))
            {
                return Ok(ToolCallVeto::Block {
                    reason: format!("Path \"{}\" is protected", truncate(path, MAX_QUOTED_PATH)),
                    terminate: false,
                });
            }
            Ok(ToolCallVeto::Continue)
        })
    }
}
