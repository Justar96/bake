# Pi agent source reference for the Rust harness

## Summary

Use the latest official Pi agent codebase to inform Bake's Rust ownership, agent-loop, and integration design. This implements [D20](scope-00/support.md#decision-register). The references below are pinned source observations, not runtime qualification or permission to change Bake's retained contracts. Custom JavaScript/Cordis profiles and their migration are excluded by [D5](scope-00/support.md#decision-register); the native plugin API remains required by D6.

## Table of Contents

- [Inspected revisions](#inspected-revisions)
- [Source map and Rust application](#source-map-and-rust-application)
- [Native MCP direction](#native-mcp-direction)
- [Other worthwhile ports](#other-worthwhile-ports)
- [Compatibility and adoption](#compatibility-and-adoption)
- [Dev Note](#dev-note)

## Inspected revisions

GitHub's official repository and latest-release endpoints were checked on 2026-10-07 UTC. The former `badlogic/pi-mono` repository redirects to `earendil-works/pi`.

| Reference | Revision | Purpose |
|---|---|---|
| [Latest stable release: v1.0.4](https://github.com/earendil-works/pi/releases/tag/v1.0.4), published 2026-10-05 at 22:03:50 UTC | `7c10bd4337495ee613f2224843ecdf349b80d1df` | Released reference and changelog |
| [Inspected main](https://github.com/earendil-works/pi/commit/503c605528f9af993c0e37ede468cf884fb0ff5b), committed 2026-10-07 at 19:05:41 UTC | `503c605528f9af993c0e37ede468cf884fb0ff5b` | Current source beyond that release; unreleased behavior must be assessed separately |

The source was fetched and its relevant files and release-to-main diff read. The first inspected main revision was `f10993bc7f28145df1375f3ff39c7f5c4cfc05f0`; a fresh check advanced the reference to `503c6055`. The MCP implementation and tests are unchanged between those two revisions. Bake's TypeScript comparison lockfile still resolves `pi-ai`, `pi-mcp`, and `pi-codemode` to 1.0.0; reviewing newer Pi code does not update that oracle. Pi dependencies were not installed and its tests were not run. Before implementing a relevant Bake scope, check the official release and main revisions again and record any new reference explicitly. A moving branch name alone is insufficient provenance.

## Source map and Rust application

All file links below refer to the inspected main commit.

| Pi owner | Observed boundary | Application to Bake |
|---|---|---|
| [`agent-loop.ts`](https://github.com/earendil-works/pi/blob/503c605528f9af993c0e37ede468cf884fb0ff5b/packages/agent/src/agent-loop.ts), [`types.ts`](https://github.com/earendil-works/pi/blob/503c605528f9af993c0e37ede468cf884fb0ff5b/packages/agent/src/types.ts) | `runAgentLoop` receives context, configuration, an event sink, cancellation, and a stream function. Turn preparation, request preparation, steering, and follow-up queues have distinct boundaries. | Scope 09: keep a small loop with explicit inputs and effects. Persist Bake's model-visible decisions before using them; retain its admission, tool settlement, and replay rules. |
| [`agent.ts`](https://github.com/earendil-works/pi/blob/503c605528f9af993c0e37ede468cf884fb0ff5b/packages/agent/src/agent.ts) | The `Agent` wrapper owns mutable execution state and drives the loop. | Scopes 05 and 09: make ownership and shutdown explicit. UI views consume projections; they must not become a second owner of agent history. |
| [`agent-session.ts`](https://github.com/earendil-works/pi/blob/503c605528f9af993c0e37ede468cf884fb0ff5b/packages/coding-agent/src/core/agent-session.ts), [`agent-session-services.ts`](https://github.com/earendil-works/pi/blob/503c605528f9af993c0e37ede468cf884fb0ff5b/packages/coding-agent/src/core/agent-session-services.ts) | Session coordination is shared across modes; `createAgentSessionServices` assembles cwd-bound services separately. The session module also imports terminal theme and HTML renderer code. | Scopes 05, 09, and 13: share the native session coordinator across headless, TUI, and Desktop. Keep rendering and mode I/O outside the core rather than copying Pi's whole session class. |
| [`stream-fn.ts`](https://github.com/earendil-works/pi/blob/503c605528f9af993c0e37ede468cf884fb0ff5b/packages/agent/src/stream-fn.ts), [`model-runtime.ts`](https://github.com/earendil-works/pi/blob/503c605528f9af993c0e37ede468cf884fb0ff5b/packages/coding-agent/src/core/model-runtime.ts) | The loop accepts a stream function; model/auth services are assembled outside it. | Scopes 06–07: isolate provider adaptation and authentication from orchestration. Keep Bake's provider support matrix and wire fixtures. |
| [`agent-loop.ts`](https://github.com/earendil-works/pi/blob/503c605528f9af993c0e37ede468cf884fb0ff5b/packages/agent/src/agent-loop.ts), [`extensions/types.ts`](https://github.com/earendil-works/pi/blob/503c605528f9af993c0e37ede468cf884fb0ff5b/packages/coding-agent/src/core/extensions/types.ts) | Tool execution separates preparation, execution, and finalization; execution stops accepting updates and awaits admitted update callbacks. Extension contributions have typed boundaries. | Scopes 08 and 12: use explicit tool lifecycle and contribution contracts. Bake's approval, sandbox, ordering, and awaited teardown remain mandatory; a native plugin API does not imply Pi JavaScript extension compatibility. |

The main revision adds monotonic tool duration measurement around `execute()` and assistant response timing in [`event-stream.ts`](https://github.com/earendil-works/pi/blob/503c605528f9af993c0e37ede468cf884fb0ff5b/packages/ai/src/utils/event-stream.ts). These are unreleased reference changes relative to v1.0.4. Evaluate timing and cancellation ownership in their native scopes; adopting a Pi field does not justify changing Bake's persisted format.

## Native MCP direction

The owner selected Pi's latest MCP implementation as the reference for scope 12. Review both its standalone [`packages/mcp`](https://github.com/earendil-works/pi/tree/503c605528f9af993c0e37ede468cf884fb0ff5b/packages/mcp) client and the coding agent's [`extensions/mcp`](https://github.com/earendil-works/pi/tree/503c605528f9af993c0e37ede468cf884fb0ff5b/packages/coding-agent/src/extensions/mcp) lifecycle. Port the relevant behavior into Rust; neither the npm package nor Pi's JavaScript extension loader becomes a runtime dependency. The client/transport split informs ownership without requiring a hand-written Rust protocol stack; qualify any Rust library against these behaviors and Bake's contracts before selecting it.

| Priority | Pi source and tests | Native acceptance target |
|---|---|---|
| Client and transport separation | `packages/mcp/src/client.ts`, `transports/{stdio,streamable-http}.ts`; `test/{client,stdio,streamable-http}.test.ts` | Request correlation, initialization, paginated tool discovery, cancellation, progress, and bounded I/O; transport tasks have explicit owners and shutdown is awaited. |
| Cancellation and OAuth shutdown | [`f10993bc` fix](https://github.com/earendil-works/pi/commit/f10993bc7f28145df1375f3ff39c7f5c4cfc05f0), included in the inspected main; `oauth/{flow,discovery}.ts`, coding-agent `extensions/mcp/index.ts`; `test/suite/agent-session-mcp-oauth.test.ts` | Cancel login during discovery, refresh, callback wait, and session teardown; join sign-in and connection work. An aborted refresh must not start a new login. Closing HTTP must not refresh tokens merely to send session DELETE. Qualify timeout values against Bake rather than inheriting them silently. |
| Connection recovery and child cleanup | `transports/streamable-http.ts`, `transports/stdio.ts`; transport tests and `test/fixtures/stubborn-server.mjs` | Exercise session expiry, reconnect/resumption, mid-stream disconnect, stalled startup, and stdio process-tree shutdown on supported OSes. Preserve Bake's reconnect budget and atomic tool generations; retire and drain old clients. Do not retry tool calls whose execution outcome is uncertain. No late tool publication after close. |
| Scoped tool and resource publication | coding-agent `extensions/mcp/{tools,resources,index}.ts`; `test/suite/agent-session-mcp.test.ts` | Integrate through Bake's native tool/policy pipeline. Keep stable `mcp__<server>__<tool>` names, scoped resource list/template/read access, bounded attributed server instructions, and replayable tool-generation changes. Preserve the pinned name algorithm, result validation, image projection, and spill limits. |

Bake's [MCP client](../../../packages/mcp/mcp-client/README.md) and [resource service](../../../packages/mcp/mcp-resources/README.md) remain compatibility inputs. Pi's generic client can inform the transport implementation, but its content conversion, tool names, exposed resource tools, and protocol subset do not define Bake's support matrix. Native configuration must expose MCP servers without Cordis; scope 05 owns that schema. Bake's current bridge does not support OAuth and requires a reload after an HTTP session expires. OAuth and automatic expired-session recovery are therefore explicit native improvements, not already-shipped parity. Test them through the native supervisor and credentials service; retain scrubbed child environments, sandbox and egress enforcement, and redaction. Pi's executable `!cmd` configuration values are excluded. Adopt its `coding-agent/test/mcp-conformance/` runner as a development reference for a native conformance arm with recorded baseline results. New OAuth behavior and intentional model-visible differences need dedicated native tests and, where applicable, paired evals. Preserve Pi's MIT notice and the MCP SDK attribution under its `packages/mcp/LICENSES/` for adapted code.

## Other worthwhile ports

These are recommended candidates within their existing scope dependencies; they do not bypass session, host, configuration, or provider prerequisites.

| Order of investigation | Candidate and source | Benefit and boundary |
|---|---|---|
| 1 | Explicit loop and mode-independent session services (`packages/agent/src/agent-loop.ts`, coding-agent `core/agent-session-services.ts`; scopes 05 and 09) | Smaller owners for execution, persistence, and UI. Adopt the boundaries while retaining Bake's session and event contracts. |
| 2 | Provider stream terminal states and retry classification (`packages/ai/src/types.ts`, `utils/{event-stream,provider-retry}.ts`; scopes 06–07) | Make cancellation and recoverable provider errors explicit. Test each classification and retry outcome; avoid unbounded queues and background tasks without an owner. |
| 3 | Tool preparation, concurrent execution, ordered settlement, and recorded tool-set changes (`packages/agent/src/agent-loop.ts`; scopes 08–09) | Clear tool lifetimes and reconstructable model requests. Persist Bake's own outcomes for every admitted call, including interruption; preserve permission and sandbox checks. |
| 4 | Confined code-mode execution (`packages/codemode/src/runtime/`, `test/sandbox.test.ts`; scope 12) | Port QuickJS execution and containment behavior with tests for cancellation, never-settling promises, infinite loops, recursion, output bounds, and nested tool cleanup. Keep Bake's bindings and model-visible output unless a separately evaluated change is adopted. |
| 5 | Shell output handling across chunk boundaries (`packages/coding-agent/src/core/bash-executor.ts`; scopes 04 and 08) | Use its split-ANSI regression as a test input for native output handling. Keep raw capture, model-visible truncation, and terminal rendering separate. |
| 6 | Terminal program status (`packages/tui/src/program-status.ts`, `test/program-status.test.ts`; scopes 14–15) | The inspected main adds OSC 7501 status reporting. Evaluate it after composer, Unicode, scrollback, and teardown qualification, with capability detection and PTY tests; it is an optional improvement, not a new 0.4.0 release gate. |

## Compatibility and adoption

Bake's accepted support matrix, Session format 3, request reconstruction, tool contracts, and native conformance evidence remain authoritative. Pi's session format, experimental durable storage, TypeScript extension loader, package manager, and terminal implementation are not compatibility targets. Ratatui/Crossterm and the Bake-owned composer remain the [terminal direction](terminal.md). Review DeepSeek Harness selectively as the existing repository rules require.

For each adopted behavior, record the exact Pi revision and source, explain the Bake requirement it serves, and verify it through the owning native scope. Changes to model-visible behavior require Bake's paired eval procedure. Retain the [MIT license and attribution](https://github.com/earendil-works/pi/blob/503c605528f9af993c0e37ede468cf884fb0ff5b/LICENSE) whenever code is copied or adapted. This reference adds no runtime dependency and closes no roadmap scope.

## Dev Note

The support register owns product decisions; this page owns the inspected Pi revisions and their application to the Rust design.
