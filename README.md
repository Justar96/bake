# Bake

English | [中文](README.zh.md)

Bake is a terminal coding agent. It streams model output, runs tools inside a sandbox, persists every session as an append-only log you can resume, and is driven entirely from the keyboard. The interface is built with Ink; the agent runs on a Cordis plugin runtime derived from DeepSeek Harness.

Bake is an independent project. Upstream releases are reviewed and ported selectively; Bake never tracks or merges upstream branches automatically.

## Install

Bake requires Node.js 24 or newer on `PATH`. Release archives exist for macOS (arm64, x64), Linux (arm64, x64), and Windows (x64); the [release manifest](https://bake.justar.dev/latest.json) lists the current version and platforms.

```sh
curl -fsSL https://bake.justar.dev/install.sh | sh
```

```powershell
irm https://bake.justar.dev/install.ps1 | iex
```

The installer verifies the manifest's Ed25519 signature and the archive's SHA-256 before it installs anything. On macOS and Linux it unpacks the release under `~/.local/share/bake` and links `bake` into `~/.local/bin`; add that directory to `PATH` if the installer asks. On Windows it installs under `%LOCALAPPDATA%\Bake` and adds its `bin` directory to the user `PATH` and to the running PowerShell session. The Unix installer needs `curl`, `tar`, and `shasum` or `sha256sum`; the Windows installer needs `tar.exe`.

Run `bake` to start a session. Authenticate with `/login`, or export `DEEPSEEK_API_KEY` before starting. Bake keeps sessions, credentials, and profiles in `~/.bake`; set `DSH_HOME` to use another directory. It never reads or migrates upstream's `~/.dsh`. `bake --help` lists the terminal options, such as `--resume <id>`.

### Update and uninstall

`bake update` downloads the latest release, verifies it the same way the installer does, and switches to it only after the new version starts. The previous version stays on disk. `bake update --check` only reports: it exits 0 when current, 10 when a newer release exists, and 1 on failure. The status line also shows when an update is available; set `BAKE_NO_UPDATE_CHECK=1` to turn that check off. Installs of 0.1.0 have no `bake update` command; rerun the installer once to move to the updatable layout.

| Variable | Effect |
|---|---|
| `BAKE_INSTALL_ROOT` | Install directory, instead of `~/.local/share/bake` or `%LOCALAPPDATA%\Bake` |
| `BAKE_BIN_DIR` | Directory for the `bake` command, instead of `~/.local/bin` or `<install root>\bin` |
| `BAKE_RELEASE_BASE_URL` | Release host; `https://github.com/Justar96/bake/releases/latest/download` installs from GitHub releases |
| `BAKE_SKIP_PATH_UPDATE=1` | Windows: leave the user `PATH` unchanged |

To uninstall, delete the install directory and the `bake` link (`~/.local/share/bake` and `~/.local/bin/bake`, or `%LOCALAPPDATA%\Bake`). Delete `~/.bake` as well to remove sessions and stored credentials.

## Run from source

Install the Bun version pinned in `package.json`, Node 24 or newer, and a C/C++ toolchain with Node headers for the native modules. The terminal test suite runs on Linux and macOS.

```sh
bun install --frozen-lockfile
bun run build
bun run start
```

`start` runs the existing build output with the same `~/.bake` home and credentials as an installed `bake`. Never commit keys or `.env`. Bun owns the workspace, lockfile, and builds; the agent itself runs on Node, because its boot loader depends on V8 internals that Bun's engine lacks.

## Develop

```sh
bun run dev                  # hot component preview; no agent or model key
bun run dev:tui              # rebuild and run the full Node agent
bun run check                # types, tests, renderer peers, layout, and docs
bun run test:e2e              # real-terminal replay; runtime must be built
bun run verify               # build + check + keyless terminal scenarios
```

The [development guide](CONTRIBUTING.md#choose-a-development-loop) covers rebuild loops, scenario filters, and troubleshooting; `bun apps/tui/scripts/tui.ts help` lists individual test targets, fixture recording, and performance diagnostics. The [release guide](distribution/README.md) covers building, signing, and publishing releases.

## Repository

- [`apps/tui/`](apps/tui/DESIGN.md): terminal application, Ink components, fixtures, and development tools.
- [`apps/cli/`](apps/cli/README.md): Node launcher for `tui` and `headless` profiles.
- [`packages/`](packages/README.md): shared agent, session, model, tool, sandbox, and plugin services.
- [`native/`](native/README.md) and [`vendor/`](vendor/README.md): native support and pinned Cordis sources.
- [`snapshots/`](snapshots/AGENTS.md): recorded session evidence, including retained historical generations.

[`CONTRIBUTING.md`](CONTRIBUTING.md) covers development and the upstream review process. The TUI's [limitations](apps/tui/DESIGN.md#10-limits) are documented with its design.

## License

MIT. Bake retains the original copyright notices and [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md). Shared runtime packages keep their `@deepseek-ai/*` names; this does not make Bake an official DeepSeek product.
