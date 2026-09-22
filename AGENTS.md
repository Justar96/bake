# Bake

Bake is an independent terminal coding agent. `origin` is Bake's repository; `upstream` is a reference for reviewing DeepSeek Harness releases and selectively porting changes. Do not merge upstream automatically or push to it.

## Workspace

Bun owns dependency installation, `bun.lock`, workspace scripts, builds, hooks, and CI. Use `bun install --frozen-lockfile` to reproduce the checkout. Do not add a root pnpm or npm lockfile. The agent process runs on Node; Bun builds and launches it. The runtime's external-profile package manager is separate from this workspace. Bake launch commands default to `~/.bake`; explicit `DSH_HOME` values select another home without migrating upstream data.

- `tui/packages/app/`: profile composition, agent control, terminal lifecycle.
- `tui/packages/ui/`: pure Ink components, projection, layout, localized copy.
- `tui/packages/harness/`: component development and recording.
- `apps/cli/`: Node profile launcher and external-plugin management.
- `packages/`: shared agent runtime and its tests; read [the architecture](docs/architecture.md) before changing it.
- `native/`, `vendor/`: native support and pinned Cordis sources. Preserve licenses and upstream attribution.
- `snapshots/`: recorded session evidence. Never overwrite, move, or delete committed session generations.

## Commands

```sh
bun install --frozen-lockfile
bun run build             # Node runtime and TUI artifacts
bun run start             # built terminal agent
bun run dev               # hot component preview, no agent or model key
bun run dev:tui           # build and run the real Node agent
bun run check             # TUI types, tests, layout, peer identity, and docs
bun run test              # pure and Node integration tests
bun run test:runtime <file>  # focused shared-runtime tests
bun run test:e2e           # keyless built-profile PTY scenarios
bun run verify            # build, check, and keyless PTY scenarios
```

Run relevant checks, not an exhaustive suite by default. Built-profile PTY scenarios are required for terminal behavior changes. Tests using Cordis or Ink run on Node; pure modules and tooling tests run on Bun. Report only commands actually run, including failures and skipped checks. Never bypass hooks without explicit approval.

## Engineering

- ESM and strict TypeScript throughout. Local relative imports use `.ts`; cross-package imports use declared package names. Retain existing `@deepseek-ai/*` identifiers when porting runtime fixes.
- Extend behavior through Cordis plugins and documented events. Registrations are effects with disposers; waterfall listeners call `next()` when delegating.
- Model-visible input must be reconstructable from the session log. Preserve released data and migration behavior; consult [session format status](docs/session-format-status.md) before persistence changes.
- Read [defensive patterns](docs/defensive-patterns.md) before lifecycle or concurrency work. Teardown must await owned work, restore terminal state, and leave no late callbacks.
- Tests own temporary paths, ports, global mutations, and subprocesses. Mock external nondeterminism, not the runtime being tested.
- Keep UI state authoritative: render logged events and runtime projections rather than maintaining competing copies. Localized product text belongs in `tui/packages/ui/src/copy.ts`.
- Remove obsolete consumers, tests, and documentation with a removed feature. Do not delete a shared package until imports, package dependencies, TypeScript references, and YAML compositions have been checked.
- Documentation describes current Bake behavior. Update the owning README and JSDoc with code changes; keep English and Chinese text aligned when editing a paired page. Historical upstream design material is reference, not an instruction to restore removed products.
- Keep comments local and explain non-obvious obligations. Files end with one newline. Never commit credentials or `.env`.
