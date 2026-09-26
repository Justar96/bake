# Bake profile launcher reference

English | [中文](README.zh.md)

The `@deepseek-ai/dsh` launcher starts Bake's Node runtime through a named Cordis profile. The shipped profiles are `tui` for the terminal agent and `headless` for one task. [Launcher overview](../README.md) owns the common commands.

<a id="profile-boot"></a>
## Profile boot

Each profile keeps its bundle list and optional `cordis.patch.yml` under `$DSH_HOME/profiles/<name>`. The launcher applies bundle, profile, home, and invocation patches in that order. First use initializes a shipped profile; an existing profile keeps its selected bundles. A missing bundle fails startup.

The launcher parses its own profile flags and forwards the remaining arguments to the application. `--from-default-profile <template>` creates a custom profile from a shipped template; `--dump-default-config` and `--dump-config` inspect composition without starting the agent.

<a id="source-execution"></a>
## Source execution

From the repository root, `bun run build` builds the Node runtime and TUI, and `bun run start` launches the built terminal agent. `bun run dsh` runs the built profile launcher. Bake uses `~/.bake` unless `DSH_HOME` selects another home. External profile package installation remains pnpm-managed and is separate from the Bun source workspace.

<a id="startup-diagnostics"></a>
## Startup diagnostics

When a required plugin fails activation, the launcher reports failed and pending plugins, missing services, and the original error. It writes a uniquely named startup report under the selected home’s `logs/` directory when possible; failure to write the report is also printed to stderr. The process exits with status 1. Raw plugin errors may contain configuration values, so review a report before sharing it.
