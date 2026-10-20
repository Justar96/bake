//! The `guard` plugin: Pi v1.3.0's protected-paths example
//! (`packages/coding-agent/examples/extensions/protected-paths.ts`, revision
//! `71023aec1546d6d677a65669c5665c0927ce0dd0`, MIT; see `NOTICE`) as a Bake
//! plugin component for `wasm32-wasip2`.
//!
//! It blocks `write` and `edit` calls whose `path` contains `.env`, `.git/`,
//! or `node_modules/`, with Pi's reason text, and logs Pi's notice text
//! (`Blocked write to protected path: …`) at warning level instead of
//! showing it in a UI. Matching is Pi's: a case-sensitive substring test on
//! the path as the model wrote it, with no normalization. It is a
//! convenience, not a confidentiality boundary: `bash` and `read` still
//! reach those paths, and Bake's permission checks remain the enforcement
//! floor. A call without a string `path` is answered with an error, which
//! fails the plugin and so blocks the call.
//!
//! Its manifest, `plugin.json`, lists `edit` and `write`. The committed
//! component is built by `rust/plugins/build-fixtures.sh`.

// Only a Wasm build exports the plugin; elsewhere the crate is checked and
// unit-tested but nothing calls it.
#![cfg_attr(not(target_family = "wasm"), allow(dead_code))]

use bake_plugin_sdk::{BlockInfo, Decision, LogLevel, Plugin, ToolCall, log};

/// Pi's `protectedPaths`.
const PROTECTED: [&str; 3] = [".env", ".git/", "node_modules/"];

/// The longest path quoted in a reason or log line, in bytes, as Bake's
/// compiled-in port quotes it.
const MAX_QUOTED_PATH: usize = 1024;

struct Guard;

impl Plugin for Guard {
    fn on_tool_call(call: ToolCall) -> Result<Decision, String> {
        if call.name != "write" && call.name != "edit" {
            return Ok(Decision::None);
        }
        let args: serde_json::Value = serde_json::from_str(&call.args_json)
            .map_err(|error| format!("{} arguments are not JSON: {error}", call.name))?;
        let Some(path) = args.get("path").and_then(serde_json::Value::as_str) else {
            return Err(format!("{} call has no string path", call.name));
        };
        if !PROTECTED.iter().any(|fragment| path.contains(fragment)) {
            return Ok(Decision::None);
        }
        let quoted = truncate(path, MAX_QUOTED_PATH);
        log(
            LogLevel::Warn,
            &format!("Blocked write to protected path: {quoted}"),
        );
        Ok(Decision::Block(BlockInfo {
            reason: format!("Path \"{quoted}\" is protected"),
            terminate: false,
        }))
    }
}

/// `text` cut to at most `max` bytes on a character boundary.
fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

// Exported only for Wasm: on the host, where the workspace's tests build
// every crate, the component's export names are not valid linker symbols.
#[cfg(target_family = "wasm")]
bake_plugin_sdk::export_plugin!(Guard);

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(name: &str, args: &str) -> Result<Decision, String> {
        Guard::on_tool_call(ToolCall {
            id: "c1".to_owned(),
            name: name.to_owned(),
            args_json: args.to_owned(),
        })
    }

    // The blocking branch calls the host's `log`, which only a Wasm build
    // can reach; the process tests cover it through `bake`.
    #[test]
    fn other_tools_and_paths_continue_and_bad_arguments_fail() {
        assert!(matches!(
            ask("read", r#"{"path":".env"}"#),
            Ok(Decision::None)
        ));
        assert!(matches!(
            ask("write", r#"{"path":"src/main.rs"}"#),
            Ok(Decision::None)
        ));
        // Pi's substring test is case-sensitive and unnormalized.
        assert!(matches!(
            ask("edit", r#"{"path":".GIT/config"}"#),
            Ok(Decision::None)
        ));
        assert!(matches!(
            ask("edit", r#"{"path":".git"}"#),
            Ok(Decision::None)
        ));
        assert_eq!(
            ask("write", r#"{"path":7}"#).unwrap_err(),
            "write call has no string path"
        );
        assert!(
            ask("edit", "not json")
                .unwrap_err()
                .starts_with("edit arguments are not JSON")
        );
    }

    #[test]
    fn quoted_paths_are_cut_on_a_character_boundary() {
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("aé", 2), "a");
    }
}
