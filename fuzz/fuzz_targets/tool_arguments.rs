//! Model-written tool arguments against a tool's JSON schema, through
//! `bake-agent`'s coercion and validator, which the agent loop runs before
//! every tool call. Both halves are untrusted: arguments come from the model
//! and schemas from third-party tools such as MCP servers.
//!
//! Input: UTF-8 text (lossy); the schema is the JSON before the first line
//! break and the arguments the JSON after it. Oracle: arguments the
//! validator accepts are a fixed point, accepted again unchanged.

#![no_main]

use bake_agent::validation::validate_tool_arguments;
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

/// The validator's message when its step budget or wall-clock limit ends a
/// call; a second call can run out where the first did not.
const TOO_COMPLEX: &str = "the schema is too complex or recursive to check";

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let Some((schema, arguments)) = text.split_once('\n') else {
        return;
    };
    let (Ok(schema), Ok(arguments)) = (
        serde_json::from_str::<Value>(schema),
        serde_json::from_str::<Value>(arguments),
    ) else {
        return;
    };
    let Ok(accepted) = validate_tool_arguments(&schema, "fuzz", &arguments) else {
        return;
    };
    match validate_tool_arguments(&schema, "fuzz", &accepted) {
        Ok(again) => assert_eq!(again, accepted, "accepted arguments are a fixed point"),
        Err(error) if error.contains(TOO_COMPLEX) => {}
        Err(error) => panic!("accepted arguments {accepted} are refused: {error}"),
    }
});
