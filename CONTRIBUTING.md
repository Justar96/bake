# Contributing to Bake

English | [中文](CONTRIBUTING.zh.md)

## First build and run

Install the Bun version pinned in `package.json`, Node 24 or newer, and a C/C++ compiler with Node development headers. Run these commands from the repository root:

```sh
bun install --frozen-lockfile
bun run build
bun run start
```

`start` runs built files without rebuilding. It requires an interactive terminal; use `/login` or `DEEPSEEK_API_KEY` for real model requests. `bun run start --help` prints the TUI flags without starting a session.

Bake's `start`, `dev:tui`, and `dsh` commands use `~/.bake` by default. Set `DSH_HOME` to an explicit directory to override it. Existing `~/.dsh` profiles and sessions are not moved or changed. An override selects that directory's existing profiles, not just its credentials.

## Choose a development loop

| Work | Command | Behavior |
| --- | --- | --- |
| Ink components | `bun run dev` | Hot-reloads a recorded component preview; no agent or network. |
| Streaming preview | `bun run dev --replay` | Plays recorded rows into the preview. |
| Chinese copy | `bun run dev --locale zh` | Uses the Chinese component dictionary. |
| Full agent | `bun run dev:tui` | Builds the runtime and TUI, then starts the Node agent; no automatic restart. |
| TUI-only edits | `bun run build:tui && bun run start` | Rebundles terminal code; requires an existing runtime build. |
| Shared runtime edits | `bun run build && bun run start` | Rebuilds runtime packages and terminal code. |

The preview accepts input for layout testing but does not submit tasks. Ctrl-C exits the preview; the real agent requires two Ctrl-C presses to quit. Stop the real agent before rebuilding its files. Never substitute `bun --bun` for the Node process: its loader uses V8 internals.

`bun run dsh --help` exposes the built profile/plugin launcher using the same Bake home. External profile-plugin installation remains separate from Bun's source workspace. Session navigation and the terminal's commands are documented in the [application README](apps/tui/packages/app/README.md).

## End-to-end checks

After installation, the complete keyless development check is:

```sh
bun run verify
```

This builds all artifacts, runs workspace validation and the TUI's types, tests, peer-identity, layout, and documentation checks, then drives the built Node profile through a real PTY. The scenarios replay recorded model responses while running real tools and checking persisted sessions, screen contents, and terminal restoration. They do not require a model key or modify your Bake home.

For shorter iterations after a runtime build:

```sh
bun run check
bun run test:e2e --list
bun run test:e2e --only rendering
bun run test:e2e --no-build
bun apps/tui/scripts/tui.ts spec packages/ui/tests/placement.spec.tsx
bun run test:runtime apps/cli/tests/args.spec.ts
```

`--only` includes a scenario's prerequisites. E2E normally rebuilds the TUI, not the shared runtime; `--no-build` uses existing artifacts unchanged. Failed scenarios report their wait condition and retain transcripts in `apps/tui/.smoke/`. `bun apps/tui/scripts/tui.ts help` lists watch modes and other diagnostic options. Run `bun run lint` for source linting.

A failed check is not a reason to refresh every snapshot or bypass hooks. Use fixed terminal streams for component snapshots and explicit interactive rendering for terminal-emulator tests; keep CI detection intact. Preserve session generations and review expected-output changes. Credentials never belong in commits.

## Upstream releases

`origin` points to Bake. `upstream` is a reference for selectively porting DeepSeek Harness fixes with their tests and license notices; do not automatically merge or push to it. Inspect package imports, TypeScript references, and profile YAML before pruning dependencies. Review session-format migrations before adopting persistence changes.

There is no automated upstream release monitor. Observing a release does not authorize applying it. Rebuild retained runtime packages and rerun their focused tests and terminal scenarios after porting a fix.
