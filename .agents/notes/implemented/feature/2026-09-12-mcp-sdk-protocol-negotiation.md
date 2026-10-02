# Agent Note: MCP SDK protocol negotiation

Status: implemented

## Problem

MCP servers use different protocol revisions. A tool bridge that implements discovery and execution around an older SDK can omit modern request headers, subscription setup, or protocol validation.

## Decision

The official SDK was later replaced by the [pi-mcp](https://github.com/earendil-works/pi) client, which [`dsh-mcp-client`](../../../../packages/mcp/mcp-client/README.md) now uses for negotiation, transports, discovery, and calls. pi-mcp negotiates protocol revisions up to 2025-11-25, so the 2026-07-28 discovery probe and `Mcp-Param` request headers are absent; discovery stops after 1,000 pages. pi-mcp checks only result envelopes, so the bridge's `src/protocol.ts` applies the MCP 2025-11-25 schema to tool definitions, tool results, and resource results, and still rejects legacy `toolResult` substitutes. `callTool` receives the raw name, arguments, signal, and timeout, without a discovered definition. The rest of this section records the SDK realization as it shipped.

`dsh-mcp-client` uses the official TypeScript client 2.0.0 with automatic protocol negotiation. The SDK owns modern discovery and legacy initialization, transport-specific negotiation, list-change subscriptions, pagination, request headers, cancellation, and output validation. The bridge uses high-level `listTools` and `callTool`, passing the complete discovered definition to each call.

The bridge retains server-qualified names, atomic registration, and durable image admission. Servers without a tools capability publish no tools. Discovery failures preserve the last successful registration; duplicate names still reject the new generation. Malformed cursor chains stop at the SDK page limit; the bridge does not add a parallel pagination implementation.

The supervisor owns each transport before the SDK attaches it to its Client. Disposal closes an unattached transport to cancel negotiation, then awaits the attempt so the SDK can reap its probe. A failed probe has no Client close event; after SDK cleanup it uses the normal retry budget. Attached connections retain the close-event barrier that prevents overlapping server processes.

The shared result adapter passes the original `ToolExecution` to each provider callback, preserving its Agent identity and cancellation. Native Cua Driver results use the SDK's public spec-type validation before canonical projection; the adapter does not accept a separate permissive result format.

The [tool bridge note](2026-07-07-mcp-client-plugin.md) retains the independent naming, scope, and image decisions. This note changes its SDK realization without replacing those decisions.

## Alternatives considered

**Keep raw requests for compatibility.** This preserves permissive malformed-result handling but bypasses SDK-owned modern headers and schema validation. Protocol-valid outputs take priority over legacy `toolResult` substitutes.

**Implement modern protocol behavior in the bridge.** This duplicates maintained SDK behavior and creates an additional compatibility implementation.

## Consequences

Stdio negotiation runs on the serving process; pi-mcp starts no probe process. Discovery stops at its page limit, and malformed results fail before projection. Valid text, canonical JSON, image admission, cancellation, and registration ownership remain bridge contracts. Shipped profiles include shared resource access; elicitation, MCP prompts, and task execution remain unsupported.

Real-transport lifecycle tests verify negotiation on one serving process, disposal during initialization, HTTP and stdio initialization retries, and the retry stop when cleanup cannot be confirmed. The connection-supervisor tests retain attached-transport close barriers and bounded failure behavior.
