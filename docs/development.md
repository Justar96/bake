# Development guide

English | [中文](development.zh.md)

This guide covers building Bake from source, the day-to-day development loops, the checks to run before a change lands, and where code lives. [`CONTRIBUTING.md`](../CONTRIBUTING.md) covers how changes are reviewed and how upstream DeepSeek Harness fixes are ported. [`AGENTS.md`](../AGENTS.md) holds the engineering rules every change follows.

## Prerequisites

- **Bun**, at the version pinned in [`package.json`](../package.json) (`packageManager`). Bun owns dependency installation, `bun.lock`, workspace scripts, builds, and Git hooks. Do not add a pnpm or npm lockfile.
- **Node.js 24 or newer.** The agent itself runs on Node, because its boot loader depends on V8 internals that Bun's engine lacks. Never substitute `bun --bun` for the Node process.
- **A C/C++ toolchain with Node headers** for the native modules.
- **Linux or macOS for the terminal scenarios.** Builds, type checks, and unit tests also run on Windows, but the PTY scenarios (`bun run test:e2e`) need Linux or macOS. WSL 2 works; keep the checkout inside the Linux filesystem and install dependencies separately there.

## First build

From the repository root:

```sh
bun install --frozen-lockfile
bun run build
bun run start
```

`bun install` also installs the Lefthook Git hooks. `start` runs the existing build output without rebuilding; it needs an interactive terminal. `bun run start --help` prints the TUI flags without starting a session.

Source runs use the same home as an installed `bake`: `~/.bake`, or the directory in `DSH_HOME`. An override selects that directory's existing profiles and sessions, not only its credentials. Existing `~/.dsh` data is never moved or changed.

For real model requests, sign in with `/login` or set `DEEPSEEK_API_KEY` in the environment or in a gitignored `.env` at the repository root. `DEEPSEEK_BASE_URL` optionally overrides the API endpoint. Never commit keys or `.env`.

## Development loops

| Work | Command | Behavior |
|---|---|---|
| Ink components | `bun run dev` | Hot-reloads a recorded component preview; no agent, network, or model key. |
| Streaming preview | `bun run dev --replay` | Plays recorded rows into the preview. |
| Chinese copy | `bun run dev --locale zh` | Uses the Chinese component dictionary. |
| Full agent | `bun run dev:tui` | Builds the runtime and TUI, then starts the Node agent; no automatic restart. |
| TUI-only edits | `bun run build:tui && bun run start` | Rebundles terminal code; needs an existing runtime build. |
| Shared runtime edits | `bun run build && bun run start` | Rebuilds runtime packages and terminal code. |

The preview accepts input for layout testing but does not submit tasks. Ctrl-C exits the preview; the real agent needs two Ctrl-C presses to quit. Stop the real agent before rebuilding its files.

`bun run dsh --help` exposes the built profile and plugin launcher with the same Bake home. External profile-plugin installation stays separate from Bun's source workspace.

## Checks

Run the checks that cover your change rather than the whole suite by default. Any terminal behavior change also needs the PTY scenarios.

```sh
bun run check          # workspace, tsconfig paths, TUI types, tests, peer identity, layout, docs
bun run test           # TUI unit and spec tests
bun run test:runtime <file-or-dir>   # focused shared-runtime tests (Vitest on Node)
bun run test:e2e       # keyless PTY scenarios against the built profile
bun run verify         # build + check + PTY scenarios
bun run lint           # Oxlint over apps/tui and scripts
```

The PTY scenarios replay recorded model responses while running real tools, then check persisted sessions, screen contents, and terminal restoration. They need no model key and do not touch your Bake home.

For shorter iterations after a runtime build:

```sh
bun run test:e2e --list
bun run test:e2e --only rendering
bun run test:e2e --no-build
bun apps/tui/scripts/tui.ts spec packages/ui/tests/placement.spec.tsx
bun run test:runtime apps/cli/tests/args.spec.ts
```

`--only` includes a scenario's prerequisites. E2E normally rebuilds the TUI, not the shared runtime; `--no-build` uses existing artifacts unchanged. Failed scenarios report their wait condition and keep transcripts in `apps/tui/.smoke/`. `bun apps/tui/scripts/tui.ts help` lists watch modes, fixture recording, and performance diagnostics.

Tests that use Cordis or Ink run on Node; pure modules and tooling tests run on Bun. A failing check is not a reason to refresh every snapshot or bypass hooks. Review expected-output changes, keep CI detection intact, and never overwrite recorded session generations under `snapshots/`.

### Git hooks

[`lefthook.yml`](../lefthook.yml) keeps the hooks fast:

- `pre-commit` lints staged TypeScript and JavaScript with Oxlint (applying fixes), rejects whitespace errors, and checks that changes under `vendor/*/src` update [`vendor/README.md`](../vendor/README.md).
- `pre-push` runs `bun run verify-workspace` and `bun run typecheck`.

The hooks do not run tests or builds; run the relevant checks yourself. Never skip hooks without the maintainer's agreement.

### CI

[`ci.yml`](../.github/workflows/ci.yml) runs the keyless checks on pushes to `main` and on pull requests. [`release.yml`](../.github/workflows/release.yml) builds, signs, and publishes release archives; the [release guide](../distribution/README.md) covers the process.

## Repository layout

| Path | Contents |
|---|---|
| [`apps/tui/`](../apps/tui/DESIGN.md) | The terminal application: `packages/app` (profile composition, agent control, terminal lifecycle), `packages/ui` (side-effect-free Ink components, projection, layout, localized copy), `packages/harness` (component development and recording), fixtures, and dev tools. |
| [`apps/cli/`](../apps/cli/README.md) | Node launcher for the `tui` and `headless` profiles, and external-plugin management. |
| [`packages/`](../packages/README.md) | The shared agent runtime: agent loop, sessions, models, tools, sandbox, and plugin services. Read the [architecture](architecture.md) before changing it. |
| [`native/`](../native/README.md), [`vendor/`](../vendor/README.md) | Native support and pinned Cordis sources. Preserve their licenses and upstream attribution. |
| [`distribution/`](../distribution/README.md) | Release packaging, signing, and the download service. |
| [`snapshots/`](../snapshots/AGENTS.md) | Recorded session evidence, including retained historical generations. |

Useful references while working in the runtime:

- [Cordis primer](cordis-primer.md): plugins, services, and events.
- [Defensive patterns](defensive-patterns.md): read before lifecycle or concurrency work.
- [Session format status](session-format-status.md): read before any persistence change.

## Conventions

- ESM and strict TypeScript throughout. Local relative imports use `.ts`; cross-package imports use declared package names. Runtime packages keep their `@deepseek-ai/*` names so upstream fixes port cleanly.
- The shared Node runtime builds through `tsconfig.host.json`; package `tsconfig.json` files reference their workspace dependencies. When you add or remove a package, update its references and run `bun run gen-workspace` and `bun run gen-tsconfig-paths`.
- Product text shown in the TUI lives in [`apps/tui/packages/ui/src/copy.ts`](../apps/tui/packages/ui/src/copy.ts), in English and Chinese.
- Documentation describes current behavior. Update the owning README or JSDoc with the code, and keep English and Chinese pages aligned.
- Mark known issues by urgency: `FIXME` blocks a release, `TODO` should be fixed soon, and `XXX` is a someday item.

### Documenting types verbatim

[Subsystem pages](subsystems/README.md) paste declarations exactly as they appear in source, with their JSDoc. Fence such a paste as ` ```ts type-equiv ` (or ` ```ts public-api ` for a class shown without implementation bodies) and register it in [`scripts/type-equiv.manifest.json`](../scripts/type-equiv.manifest.json) with its source file and symbol:

```json
{ "doc": "docs/subsystems/session.md", "symbol": "SessionEvent", "source": "packages/core/session/src/types.ts" }
```

`bun run verify-type-equiv` compares each block, and its Chinese counterpart, with the source declaration. When you change a documented declaration, update the paste in both languages and rerun the check.
