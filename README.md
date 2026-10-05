# Bake

Bake is a keyboard-driven coding agent for the terminal. It streams the model's answer as it arrives, runs shell and file tools inside a sandbox, and saves every session as an append-only log you can resume.

- **Slash commands** for models and reasoning effort, sign-in, sessions, goals, compaction, permissions, and settings, with argument completion.
- **Long-running work**: goals that continue across rounds, subagents you can watch from the input row, and shell commands left running in the background.
- **Sandboxed tools**: bash or PowerShell, file reads and edits, search, and web search and fetch, each gated by a permission preset.
- **Sessions you keep**: resume by id or pick from a list; context is compacted when it fills up, at a threshold you can set.
- **Skills and attachments**: invoke project or user skills as `/name`, and attach images and files to a prompt.

## Install

Bake needs **Node.js 24 or newer** on `PATH`. Releases are published for macOS (arm64, x64), Linux (arm64, x64), and Windows (x64).

**macOS and Linux**

```sh
curl -fsSL https://bake.justar.dev/install.sh | sh
```

**Windows (PowerShell)**

```powershell
irm https://bake.justar.dev/install.ps1 | iex
```

The installer checks the release manifest's Ed25519 signature and the archive's SHA-256 before it installs anything.

| Platform | Installs to | `bake` command | Needs |
|---|---|---|---|
| macOS, Linux | `~/.local/share/bake` | linked into `~/.local/bin` (add it to `PATH` if asked) | `curl`, `tar`, `shasum` or `sha256sum` |
| Windows | `%LOCALAPPDATA%\Bake` | `bin` added to the user `PATH` and the current session | `tar.exe` |

<details>
<summary>Installer options</summary>

| Variable | Effect |
|---|---|
| `BAKE_INSTALL_ROOT` | Install directory, instead of `~/.local/share/bake` or `%LOCALAPPDATA%\Bake` |
| `BAKE_BIN_DIR` | Directory for the `bake` command, instead of `~/.local/bin` or `<install root>\bin` |
| `BAKE_RELEASE_BASE_URL` | Release host; `https://github.com/Justar96/bake/releases/latest/download` installs from GitHub releases |
| `BAKE_SKIP_PATH_UPDATE=1` | Windows: leave the user `PATH` unchanged |

The [release manifest](https://bake.justar.dev/latest.json) lists the current version and platforms.

</details>

## Get started

```sh
bake
```

Bake has no default provider: sign in with `/login` (DeepSeek, CLIProxyAPI, OpenAI, Anthropic, GitHub Copilot, OpenRouter, Kimi, or xAI), and the first sign-in selects its model. An exported `DEEPSEEK_API_KEY` counts as signed in; choose its model with `/model`. Type `/` to browse commands, `/help` for the list, and `@` to reference a file. `/settings` changes the screen mode, the default model and permissions, compaction, and tool limits. `bake --help` shows the launch options, such as `--resume <id>`.

Bake keeps sessions, credentials, and profiles in `~/.bake`. Set `BAKE_HOME` to use a different directory; the earlier `DSH_HOME` name still works when `BAKE_HOME` is unset, as does each `DSH_` spelling of a `BAKE_` setting.

## Update

```sh
bake update
```

`bake update` downloads the latest release, verifies it the same way the installer does, and switches only after the new version starts. The previous version stays on disk; older releases no `bake` has started for a week are removed. `bake update --check` only reports: it exits 0 when current, 10 when a newer release exists, and 1 on failure. The status line also shows when an update is available, checking hourly and again ten minutes after a failed check; run `/update` in the terminal to install it without leaving the session. The download service counts each install that checks as one active install that day: it sends a salted hash of the requesting address, which changes daily, to its activity counter at gissx.org, and the address itself never leaves the service. Set `BAKE_NO_UPDATE_CHECK=1` to turn the background check off, and with it the daily count; `bake update` and `/update` still contact the service when you run them.

Installs of 0.1.0 have no `bake update`; run the installer once more to move to the updatable layout.

## Uninstall

Delete the install directory and the `bake` link: `~/.local/share/bake` and `~/.local/bin/bake`, or `%LOCALAPPDATA%\Bake` on Windows, where you also remove its `bin` entry from the user `PATH`. Delete `~/.bake` too to remove sessions and stored credentials.

## Documentation

- [Safety](SAFETY.md): what the sandbox does and does not protect.
- [Terminal application](apps/tui/packages/app/README.md): commands, keys, and session navigation.
- [TUI design](apps/tui/DESIGN.md), including its [known limits](apps/tui/DESIGN.md#10-limits).
- [Development guide](docs/development.md): building from source, test loops, and repository layout.
- [Contributing](CONTRIBUTING.md) and the [changelog](CHANGELOG.md).

## Acknowledgements

Bake is a fork of [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness), DeepSeek's MIT-licensed agent harness. Its agent loop, session log, sandbox, and Cordis plugin runtime are still the foundation Bake runs on. Since the fork, Bake has built its own terminal interface and distribution, dropped upstream's web client, desktop app, and SDKs, and reworked much of the runtime; upstream fixes are reviewed and ported selectively, never merged automatically. Runtime packages under `packages/` use `bake-<name>`; the CLI, terminal, and vendored packages retain their declared names. The [contributor guide](CONTRIBUTING.md#upstream-deepseek-harness) describes the import mapping for upstream fixes.

Bake reaches every model through [pi](https://github.com/earendil-works/pi), also MIT-licensed: `pi-ai` connects each provider, `pi-codemode` runs code mode's scripts, and `pi-mcp` connects MCP servers.

Thank you to the authors of both projects.

Bake is an independent project. It is not an official DeepSeek product, and it is not endorsed by DeepSeek or by pi's authors. "DeepSeek Harness" is a trademark of DeepSeek; see its [brand guidelines](BRAND_GUIDELINES.md).

## License

[MIT](LICENSE). Bake keeps the original copyright notice and the upstream attributions in [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).
