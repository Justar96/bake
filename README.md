# Bake

English | [中文](README.zh.md)

Bake is a terminal coding agent with streaming responses, tool execution, persistent sessions, and keyboard-driven interaction. It uses Ink for presentation and a Cordis plugin runtime derived from DeepSeek Harness.

Bake is an independent project. Upstream releases are reviewed for selective adoption; Bake does not automatically track or merge upstream branches.

## Run from source

Install the Bun version pinned in `package.json`, Node 24 or newer, and a C/C++ compiler with Node development headers. The current terminal checks run on Linux and macOS.

```sh
bun install --frozen-lockfile
bun run build
bun run start
```

`start` runs existing build output and stores Bake data in `~/.bake` unless `DSH_HOME` is set. It leaves upstream's `~/.dsh` data untouched. Use `/login` in the terminal or set `DEEPSEEK_API_KEY` in your environment. Do not commit keys or `.env`. `bun run start --help` shows the terminal application's options.

Bun owns the workspace and lockfile. The built agent runs on Node because its boot loader uses Node internals. External profile-plugin installation uses the runtime's separate package-management configuration.

## Develop

```sh
bun run dev                  # hot component preview; no agent or model key
bun run dev:tui              # rebuild and run the full Node agent
bun run check                # types, tests, renderer peers, layout, and docs
bun run test:e2e              # real-terminal replay; runtime must be built
bun run verify               # build + check + keyless terminal scenarios
```

See the [development guide](CONTRIBUTING.md#choose-a-development-loop) for rebuild commands, scenario filters, and troubleshooting. `bun tui/scripts/tui.ts help` lists individual test targets, fixture recording, and performance diagnostics.

## Repository

- [`tui/`](tui/DESIGN.md): terminal application, Ink components, fixtures, and development tools.
- [`apps/cli/`](apps/cli/README.md): Node launcher for `tui` and `headless` profiles.
- [`packages/`](packages/README.md): shared agent, session, model, tool, sandbox, and plugin services.
- [`native/`](native/README.md) and [`vendor/`](vendor/README.md): native support and pinned Cordis sources.
- [`snapshots/`](snapshots/AGENTS.md): recorded session evidence, including retained historical generations.

[`CONTRIBUTING.md`](CONTRIBUTING.md) covers development and reviewing upstream releases. The TUI's [limitations](tui/DESIGN.md#10-limits) are documented with its design.

## License

MIT. Bake retains the original copyright notices and [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md). Shared runtime package identifiers retain their `@deepseek-ai/*` names; they do not imply that Bake is an official DeepSeek product.
