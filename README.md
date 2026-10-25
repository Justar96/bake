# Bake

Bake is a keyboard-driven coding agent for the terminal. It streams the model's answer as it arrives, runs shell and file tools inside a sandbox under a permission preset you choose, and saves every session so you can continue, resume, or fork it later. Bake is a single executable, `bake`.

> [!NOTE]
> **Bake 0.4.0 runs on Linux**, x86_64 and arm64, with glibc 2.34 or newer (RHEL 9 and its rebuilds, Amazon Linux 2023, Ubuntu 22.04, Debian 12, and newer). **macOS and Windows support is coming in a later 0.4 release.** Until then the installer stops on those platforms before it downloads anything; on Windows, install Bake inside WSL.
>
> 0.4 is a new install, not an upgrade of Bake 0.3, the earlier TypeScript agent. A 0.3 install keeps working with its own files, but 0.3 receives no further fixes ([D10 and D13](docs/roadmap/rust-0.4/scope-00/support.md#decision-register)).

- **Interactive sessions**, fullscreen or inline in your terminal's scrollback: steer a running turn, stop it with Esc, and pick models, thinking levels, sessions, and settings from slash commands.
- **Sandboxed tools**: bash, file reads and edits, search, web search and fetch, and code mode, which runs a JavaScript program that calls the other tools, each gated by a permission preset and approvals.
- **Sessions you keep**: continue the last one with `bake -c`, pick one with `bake -r`, fork, branch, and compact when the context fills.
- **Headless runs**: `bake -p` prints one answer; `--mode json` streams every event and `--mode rpc` speaks a JSONL protocol.
- **Extensions**: `AGENTS.md` context files, skills, prompt templates, MCP servers, command hooks, and Wasm plugins that can veto tool calls.

## Install

```sh
curl -fsSL https://bake.justar.dev/install.sh | sh
```

The installer verifies the release manifest's Ed25519 signature and the archive's SHA-256, runs the new release's self-check, and only then makes it current. It needs `curl` or `wget`, `tar`, `sha256sum` or `shasum`, and an OpenSSL with Ed25519 (1.1.1 or newer); it needs no Node or Bun.

| Installs to | `bake` command |
|---|---|
| `~/.local/share/bake-native` | linked into `~/.local/bin` (add it to `PATH` if asked) |

If `bake` already runs Bake 0.3, the installer takes that command over and records how to restore it; 0.3's `dsh` command is left alone. A `bake` command that no Bake installer made is left in place unless you pass `--replace-command`.

<details>
<summary>Installer options</summary>

Pass options to a piped install after `sh -s --`, for example `curl -fsSL https://bake.justar.dev/install.sh | sh -s -- --replace-command`.

| Option or variable | Effect |
|---|---|
| `--replace-command`, `BAKE_REPLACE_COMMAND=1` | Replace a `bake` command this installer did not make, keeping a backup it can restore |
| `--uninstall-links` | Restore the commands the installer replaced and remove the links it made |
| `BAKE_NATIVE_ROOT` | Install directory, instead of `~/.local/share/bake-native` |
| `BAKE_BIN_DIR` | Directory for the `bake` command, instead of `~/.local/bin` |
| `BAKE_OPENSSL` | The `openssl` that verifies the release signature |

</details>

## Get started

```sh
cd your-project
bake
```

Bake reaches models through your providers. Run `/login` in a session to connect a [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) account, or describe any OpenAI- or Anthropic-compatible provider in `~/.bake/models.json` with its key in `auth.json`; `bake docs models` shows both. If you used Bake 0.3, its CLIProxyAPI route, key, and default model are read from `~/.bake` without changing them. Then:

- Type a prompt and press Enter; `@` references a file and `/` lists the commands, such as `/model`, `/thinking`, `/resume`, `/compact`, and `/settings`.
- `bake -p "explain this repository"` prints one answer and exits.
- `bake --inline` keeps the session in your terminal's scrollback instead of the full screen.
- `bake --permission workspace-write` or `danger-full-access` picks the permission preset; `bake docs permissions` explains them.
- `bake docs` lists the built-in documentation, and `bake --help` the launch options.

Bake keeps sessions, settings, and credentials in `~/.bake`. Set `BAKE_HOME` to use another directory; the earlier `DSH_HOME` name still works when `BAKE_HOME` is unset.

## Update

```sh
bake update
```

`bake update` installs the newest signed release and switches to it only after its self-check passes; anything that fails before then leaves the install as it was. `bake update --check` only reports, exiting 10 when a newer release is published, and `bake update --rollback` makes the release the last update replaced current again. `bake docs update` describes each step.

## Uninstall

```sh
curl -fsSL https://bake.justar.dev/install.sh | sh -s -- --uninstall-links
rm -rf ~/.local/share/bake-native
```

The first command restores any `bake` command the installer replaced and removes the links it made. Delete `~/.bake` too to remove sessions and stored credentials; a Bake 0.3 install shares that directory.

## Documentation

- `bake docs <topic>`: the command line, the Bake home, models, settings, permissions, sessions, context files, skills, MCP, hooks, plugins, code mode, RPC, and updates.
- [Safety](SAFETY.md): what the sandbox does and does not protect.
- [Build and test Bake](docs/development.md): building from source with Cargo, the crates, and their checks.
- [0.4 roadmap](docs/roadmap/rust-0.4/README.md): scopes, decisions, and evidence, and what the macOS and Windows releases still need.
- [Contributing](CONTRIBUTING.md) and the [changelog](CHANGELOG.md).

## License

[MIT](LICENSE). Copyright © 2026 Bake contributors. Upstream copyright and license notices are preserved in `LICENSE`, crate notices, and [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).

## Acknowledgements

Bake ports [pi](https://github.com/earendil-works/pi), which is MIT-licensed: its agent loop, session format, tools, and coding agent are the foundation `bake` is built on, with Bake's own sandbox, permissions, terminal interface, and distribution. Bake began as a fork of [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness), DeepSeek's MIT-licensed agent harness, which the 0.3 TypeScript agent ran on.

Thank you to the authors of both projects.

Bake is an independent project. It is not an official DeepSeek product, and it is not endorsed by DeepSeek or by pi's authors. "DeepSeek Harness" is a trademark of DeepSeek.
