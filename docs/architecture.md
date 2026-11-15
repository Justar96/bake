# Bake architecture

`bake-cli` starts interactive or print mode. Both construct an `AgentSession` in `bake-coding-agent`, which owns tools, settings, authentication, context, compaction, MCP, and plugin integration. Pi-format session storage, the Bake home, and the locked JSON files behind settings and credentials are `bake-session`'s, which depends on no crate above `bake-agent`. `bake-agent` drives model/tool steps and queues input; `bake-ai` owns provider protocols and streaming events.

The terminal owner in `bake-tui` applies terminal effects, owns the live session, and awaits its workers on exit. `bake-tui-view` keeps editor state, input handling, projection, and rendering pure. The preview supplies scripted events through the same view boundary. Terminal state must be restored after cancellation, a failed write, and normal exit.

`bake-sandbox` owns confinement and `bake-process` owns spawned process trees. Tool execution crosses the permission and approval boundary before running commands or writing files. Teardown awaits owned work and stops descendants. `bake-codemode` hosts JavaScript tool calls, `bake-mcp` connects external tool servers, and the Wasm plugin boundary constrains plugins to the granted host API. `bake-update` verifies and switches signed installed releases.

The [workspace crate table](development.md#crates) names each crate. User documentation is embedded from [the CLI docs](../crates/bake-cli/docs/). [Direction](direction.md) lists the contracts Bake keeps as its own: the CLIProxyAPI route, permissions, and the Bake home. Cargo builds and tests the executable and its Rust workspace.
