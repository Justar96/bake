# Bake

English | [中文](README.zh.md)

Bake is a keyboard-driven coding agent for the terminal. It streams the model's answer as it arrives, runs shell and file tools inside a sandbox, and saves every session as an append-only log you can resume.

- **Slash commands** for models, sign-in, sessions, goals, plan mode, compaction, and permissions, with argument completion.
- **Long-running work**: goals that continue across rounds, a task list, subagents, and parallel workflows.
- **Sandboxed tools**: bash or PowerShell, file reads and edits, search, and web fetch, each gated by a permission preset.
- **Sessions you keep**: resume by id or pick from a list; context is compacted when it fills up.
- **Skills and attachments**: invoke project or user skills as `/name`, and attach images and files to a prompt.

Bake is built on the [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) agent runtime. See [Acknowledgements](#acknowledgements).

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

Sign in with `/login`, or export `DEEPSEEK_API_KEY` before starting. Type `/` to browse commands, `/help` for the list, and `@` to reference a file. `bake --help` shows the launch options, such as `--resume <id>`.

Bake keeps sessions, credentials, and profiles in `~/.bake`. Set `DSH_HOME` to use a different directory. Bake never reads or migrates DeepSeek Harness's `~/.dsh`.

## Update

```sh
bake update
```

`bake update` downloads the latest release, verifies it the same way the installer does, and switches only after the new version starts. The previous version stays on disk. `bake update --check` only reports: it exits 0 when current, 10 when a newer release exists, and 1 on failure. The status line also shows when an update is available; set `BAKE_NO_UPDATE_CHECK=1` to turn that check off.

Installs of 0.1.0 have no `bake update`; run the installer once more to move to the updatable layout.

## Uninstall

Delete the install directory and the `bake` link: `~/.local/share/bake` and `~/.local/bin/bake`, or `%LOCALAPPDATA%\Bake` on Windows. Delete `~/.bake` too to remove sessions and stored credentials.

## Documentation

- [Safety](SAFETY.md): what the sandbox does and does not protect.
- [Terminal application](apps/tui/packages/app/README.md): commands, keys, and session navigation.
- [TUI design](apps/tui/DESIGN.md), including its [known limits](apps/tui/DESIGN.md#10-limits).
- [Development guide](docs/development.md): building from source, test loops, and repository layout.
- [Contributing](CONTRIBUTING.md) and the [changelog](CHANGELOG.md).

## Acknowledgements

Bake is built on [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness), DeepSeek's MIT-licensed agent harness. Its agent loop, session log, sandbox, tools, and Cordis plugin runtime are the foundation Bake runs on, and we are grateful to its authors. Bake adds its own terminal interface, distribution, and changes; upstream fixes are reviewed and ported selectively, never merged automatically.

Bake is an independent project. It is not an official DeepSeek product and is not endorsed by DeepSeek. "DeepSeek Harness" is a trademark of DeepSeek; see its [brand guidelines](BRAND_GUIDELINES.md). Shared runtime packages keep their original `@deepseek-ai/*` names so upstream fixes stay easy to port.

## License

[MIT](LICENSE). Bake keeps the original copyright notice and the upstream attributions in [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).
