# The Bake home

The Bake home is the directory that holds bake's models, keys, settings, and saved sessions. It is `BAKE_HOME` when that is set and not blank, else `~/.bake`. A leading `~` in `BAKE_HOME` is expanded, and a relative path is taken from the working directory. When neither can be resolved, bake stops with an error.

`BAKE_OFFLINE` set to `1`, `true`, or `yes` does what `--offline` does: no startup network operation runs (`bake docs models`).

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

bake never writes `models.json` or `mcp.json`. Interactive commands write `auth.json` (the CLIProxyAPI key) and `settings.json` (the CLIProxyAPI route, saved defaults, and `/settings` changes), each through a locked, atomic replacement that keeps other members (`bake docs models`). It creates `sessions/`, the session files in it, and a `.lock` file beside each session it writes, which it leaves in place (`bake docs sessions`). An interactive session started from a native install writes `update-check.json`, the last published version it found and when, through an atomic replacement (`bake docs update`). The `bake plugin` commands write `plugins/`, and a session records a plugin's failure in `plugins/plugins.json` (`bake docs plugins`).
