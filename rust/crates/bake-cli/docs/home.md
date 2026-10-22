# The Bake home

The Bake home is the directory that holds bake's models, keys, settings, and saved sessions. It is the first of these that is set and not blank: `BAKE_HOME`, then `DSH_HOME`, then `~/.bake`. A leading `~` in either variable is expanded, and a relative path is taken from the working directory. When none of them can be resolved, bake stops with an error.

`BAKE_OFFLINE`, then `DSH_OFFLINE`, set to `1`, `true`, or `yes`, does what `--offline` does: no startup network operation runs (`bake docs models`).

## What bake reads there

| Path | Contents |
|---|---|
| `models.json` | Providers and models (`bake docs models`) |
| `auth.json` | API keys by provider (`bake docs models`) |
| `settings.json` | Default model, tools, sessions, compaction, retry, permission presets, and the hooks files to run (`bake docs settings`, `bake docs hooks`) |
| `sessions/` | Saved sessions, one directory per working directory (`bake docs sessions`) |
| `mcp.json` | MCP servers each session starts, unless `--no-mcp` is given (`bake docs mcp`) |
| `plugins/` | Installed Wasm plugin packages and `plugins.json`, which enables them (`bake docs plugins`) |

Instructions and skills for every project are not in the Bake home: they are in `~/.agents` in your home directory (`bake docs context` and `bake docs skills`).

bake never writes `models.json` or `mcp.json`. Interactive commands write `auth.json` (the CLIProxyAPI key) and `settings.json` (the CLIProxyAPI route, saved defaults, and `/settings` changes), each through a locked, atomic replacement that keeps other members (`bake docs models`). It creates `sessions/`, the session files in it, and a `.lock` file beside each session it writes, which it leaves in place (`bake docs sessions`). The `bake plugin` commands write `plugins/`, and a session records a plugin's failure in `plugins/plugins.json` (`bake docs plugins`).

## Settings from Bake 0.3

A home that Bake 0.3 used also holds `settings.yaml` and `.credentials.yaml`. bake reads them at startup, read-only, and never changes them:

- the CLIProxyAPI route saved by the 0.3 login (`llm-pi-ai.providers.cliproxyapi`), which becomes the `cliproxyapi` provider while `settings.json` has no `cliproxyapi` member (a `/login` or `/logout` adds one);
- that route's API key: the `CLIPROXYAPI_API_KEY` environment variable when it is set and not empty, else the key `.credentials.yaml` stores;
- the default model (`agent-default-model`) and its reasoning effort, used only when `settings.json` names no default provider or model; the effort is the default thinking level unless `defaultThinkingLevel` sets one;
- the default permission preset (`permission.defaultPreset`), used only when `settings.json` names none.

`.credentials.yaml` must be readable by its owner only (mode `0600` on Unix). A problem with it, or a `CLIPROXYAPI_API_KEY` that cannot be used, prints a warning and drops the whole import: the route, its key, and the default model. A `settings.yaml` that cannot be read or parsed does the same. bake reads nothing else from a 0.3 home, and it does not open 0.3 Session logs.
