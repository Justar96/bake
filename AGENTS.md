# Bake

Bake is an independent terminal coding agent. `origin` is Bake's own repository. `upstream` is a read-only reference: review its DeepSeek Harness releases there, then port changes selectively. Never merge from or push to `upstream` automatically.

## Workspace

Bun is the single toolchain for this workspace: it owns dependency installation, `bun.lock`, workspace scripts, builds, hooks, and CI. Reproduce a checkout with `bun install --frozen-lockfile`, and do not add a root pnpm or npm lockfile. The agent process itself runs on Node, with Bun building and launching it. The runtime's external-profile package manager is a separate concern and does not belong to this workspace. Bake launch commands default to `~/.bake`; setting `DSH_HOME` selects a different home instead, without migrating upstream data.

- `apps/tui/packages/app/`: profile composition, agent control, terminal lifecycle.
- `apps/tui/packages/ui/`: side-effect-free Ink components, projection, layout, localized copy.
- `apps/tui/packages/harness/`: component development and recording.
- `apps/cli/`: Node profile launcher and external-plugin management.
- `packages/`: shared agent runtime and its tests; read [the architecture](docs/architecture.md) before changing it.
- `native/`, `vendor/`: native support and pinned Cordis sources. Preserve licenses and upstream attribution.
- `snapshots/`: recorded session evidence. Never overwrite, move, or delete a committed session generation.

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

Run the checks relevant to your change rather than the full suite by default. Any terminal behavior change also requires the built-profile PTY scenarios. Tests that use Cordis or Ink run on Node; pure modules and tooling tests run on Bun. Report only what you actually ran, including failures and skipped checks. Never bypass hooks without explicit approval.

## Engineering

- Use ESM and strict TypeScript throughout. Local relative imports use `.ts`; cross-package imports use declared package names. When porting runtime fixes, keep the existing `@deepseek-ai/*` identifiers.
- Extend behavior through Cordis plugins and documented events, not ad hoc hooks. Registrations are effects that must supply disposers; waterfall listeners call `next()` when delegating.
- Model-visible input must be reconstructable from the session log. Preserve released data and migration behavior; consult [session format status](docs/session-format-status.md) before any persistence change.
- Read [defensive patterns](docs/defensive-patterns.md) before lifecycle or concurrency work. Teardown must await owned work, restore terminal state, and leave no late callbacks.
- Tests own their temporary paths, ports, global mutations, and subprocesses. Mock external nondeterminism, never the runtime under test.
- Keep UI state authoritative: render logged events and runtime projections instead of maintaining competing copies. Localized product text belongs in `apps/tui/packages/ui/src/copy.ts`.
- When removing a feature, also remove its obsolete consumers, tests, and documentation. Before deleting a shared package, check imports, package dependencies, TypeScript references, and YAML compositions.
- Documentation describes current Bake behavior. Update the owning README and JSDoc alongside code changes, and keep English and Chinese text aligned when editing a paired page. Historical upstream design material is reference only, never an instruction to restore removed products.
- Keep comments local and explain non-obvious obligations. Files end with one newline. Never commit credentials or `.env`.
