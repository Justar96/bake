# Bake terminal UI

Read [DESIGN.md](DESIGN.md) before changing agent wiring and [DESIGN-LAYOUT.md](DESIGN-LAYOUT.md) before changing terminal geometry. Bake owns the whole repository; root [AGENTS.md](../../AGENTS.md) governs the Bun workspace and shared runtime.

## Runtime and state

The agent runs on Node. Shipped code must not import `bun:*` or use `Bun.*`; Bun is for builds, pure tests, component development, and the PTY driver.

`packages/ui` is pure over props: no Cordis context, I/O, clocks, or Node built-ins. Put runtime effects in `packages/app`. Read agent status and session projections from their owning services, and derive the transcript from the session log. Never infer agent activity from turn events or keep a second pending-input list.

Cordis `inject` entries are required services. Use `ctx.get(name)` for optional services, wait for Loader settlement before reading the assembled tree, and register contributions through effects. Approval listeners answer only for owned agents and call `next()` otherwise.

## Terminal ownership

The runner restores terminal state through one idempotent release path reached on normal exit, Cordis disposal, and fail-loud startup failure. Keep all three paths tested. Ink owns raw mode, bracketed paste, and cursor restoration. Do not write to stdout outside the renderer; diagnostics go to stderr.

Committed transcript rows cannot be retracted. Handle session rewrites separately from append events. Keep the live region within the terminal's row budget so Ink does not repaint the full transcript during streaming.

## Validation

`bun run check` runs React peer identity, strict application/test/tooling types, pure Bun tests, Node component/integration tests, layout checks, and Markdown links. `bun run test:e2e` builds the TUI and drives the Node profile through a real PTY using recorded model output.

`bun run verify` builds first, then runs all these checks and keyless PTY scenarios. Use `bun tui/scripts/tui.ts help` for selectable targets and scenario filters. Built runtime dependencies must exist before component/integration and PTY checks. The source launch is not sufficient evidence for tool execution; use the built profile. A piped process cannot qualify rendering or terminal restoration.

TTY-emulator tests must request Ink's interactive mode explicitly; never remove `CI` from the environment to make rendering pass. Tests using real Cordis services or the product's Ink components run on Node; the Bun preview has its own subprocess smoke. Pure `.test.ts` cases run on Bun. Every terminal behavior change needs an owning expected-output test and a built-profile scenario. Keep recorded session generations intact.
