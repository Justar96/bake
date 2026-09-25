# Bake architecture

English | [中文](architecture.zh.md)

Bake is a terminal application over a Cordis plugin runtime. Read this map before changing shared packages; the [Cordis primer](cordis-primer.md) explains service injection, typed events, and reversible effects.

## Application launch

Bun manages the source workspace and builds the application. Supported Node application launches go through `dsh` profiles. The shipped profiles are `tui`, for interactive terminal sessions, and `headless`, for one task. Custom composition remains a profile plus ordered patches, not an additional executable or inline application tree.

The terminal profile stacks [`dsh-base`](../packages/bundle/base/README.md) and [`@dsh-tui/app`](../apps/tui/packages/app/README.md). Base supplies models, tools, persistence, sandbox policy, settings, and credentials. The TUI supplies agent selection, terminal ownership, user input, and presentation. The [launcher](../apps/cli/README.md) owns profile initialization and argument forwarding.

Each profile lists its bundles under `dsh.profile.bundles`. Composition applies bundle patches, the profile patch, the home patch, and invocation patches in that order. YAML controls HMR. Existing profile manifests retain their user-selected bundles.

## Shared runtime

| Owner | Responsibility |
|---|---|
| [`core/session`](../packages/core/session/README.md) | Append-only session events and model-history projection |
| [`core/agent`](../packages/core/agent/README.md) | Agent handles, registry, and typed lifecycle events |
| [`core/agent-loop`](../packages/core/agent-loop/README.md) | Turn execution and tool scheduling |
| [`core/tools`](../packages/core/tools/README.md) | Scoped tool registration and execution |
| [`core/system-prompt`](../packages/core/system-prompt/README.md) | Prompt and tool-schema assembly |
| [`llm/llm`](../packages/llm/llm/README.md) | Provider-independent model requests and streams |
| [`session/session-projection`](../packages/session/session-projection/README.md) | Incremental, authoritative session state |
| [`boot/app-boot`](../packages/boot/app-boot/README.md) | Profile loading, Node setup, and startup failure handling |

Plugins register services and listeners through effects. A capability has a service definition, a provider, and consumers; keep each role complete. Add behavior through the owning plugin or typed event rather than changing the loop for an individual tool.

## Events and turns

Session events record durable facts. Agent events describe live work. Capability events connect providers, tools, and policy. Waterfall listeners must call `next()` when delegating; returning without it stops the chain.

A turn claims queued input, assembles a request, runs a model step, executes any tools, and repeats while work remains. `agent/pre-step` can reject or rewrite claimed input. Request preparation resolves the route before committing system and user messages; cancellation before admission commits neither. The [agent-loop README](../packages/core/agent-loop/README.md) owns ordering, retry, cancellation, and disposal details.

Assistant streams are live while running. Completed messages and settled failed attempts retain compact stream data in the session log. The TUI renders committed history plus transient stream state; it does not assemble model requests or maintain a separate message history.

## Sessions and persistence

Anything visible to the model must be reconstructable from the session log. A new model-visible input requires a durable event. Session consumers use the current logical format; the JSONL provider selects stored generations and applies supported adjacent migrations before returning events.

Never move, overwrite, or delete committed generations. A migrated write publishes a version-named successor beside unchanged predecessors. Future or unsupported formats fail rather than silently falling back. [Session format status](session-format-status.md) and [JSONL persistence](../packages/session/session-persistence-jsonl/README.md) own the data rules.

The TUI reads agent activity from `agent.status`, pending input from the inbox projection, and context usage from the runtime's context-pressure projection. It must not infer these facts from turn events or keep competing reducers. See [TUI wiring](../apps/tui/DESIGN.md).

## Terminal ownership

The application runner owns one displayed session and the terminal's release path. Ink manages raw mode, bracketed paste, and cursor restoration. Normal exit, Cordis disposal, and fatal startup failure all release terminal resources. Teardown waits for owned work to settle.

The live region stays within the terminal row budget; committed transcript rows remain in scrollback. Pure Ink components receive props and localized strings. Agent access, filesystem reads, and other effects stay in the application package. [Layout design](../apps/tui/DESIGN-LAYOUT.md) owns terminal geometry and rendering rules.

## Upstream adoption

Bake retains shared runtime package names and session evidence to support selective upstream fixes. The [contributor guide](../CONTRIBUTING.md#upstream-deepseek-harness) owns release review. Upstream's Web, Desktop, SDK, and publication workflows are not Bake application requirements.
