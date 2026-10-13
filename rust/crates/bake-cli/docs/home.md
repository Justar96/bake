# The Bake home

The Bake home is the directory that holds bake-rs's models, keys, settings, global instructions, and saved sessions. It is the first of these that is set and not blank: `BAKE_HOME`, then `DSH_HOME`, then `~/.bake`. A leading `~` in either variable is expanded, and a relative path is taken from the working directory. When none of them can be resolved, bake-rs stops with an error.

## What bake-rs reads there

| Path | Contents |
|---|---|
| `models.json` | Providers and models (`bake-rs docs models`) |
| `auth.json` | API keys by provider (`bake-rs docs models`) |
| `settings.json` | Default model, tools, sessions, compaction, retry, and permission presets (`bake-rs docs settings`) |
| `AGENTS.md` | Instructions for every project (`bake-rs docs context`) |
| `sessions/` | Saved sessions, one directory per working directory (`bake-rs docs sessions`) |

bake-rs never writes `models.json`, `auth.json`, or `settings.json`. It creates `sessions/` and the session files in it.

## Settings from Bake 0.3

A home that Bake 0.3 used also holds `settings.yaml` and `.credentials.yaml`. bake-rs reads them at startup, read-only, and never changes them:

- the CLIProxyAPI route saved by the 0.3 login (`llm-pi-ai.providers.cliproxyapi`), which becomes the `cliproxyapi` provider;
- that route's API key: the `CLIPROXYAPI_API_KEY` environment variable when it is set and not empty, else the key `.credentials.yaml` stores;
- the default model (`agent-default-model`), used only when `settings.json` names no default provider or model;
- the default permission preset (`permission.defaultPreset`), used only when `settings.json` names none.

A file that cannot be read or parsed produces a warning, and the run continues without what it would have supplied. bake-rs reads nothing else from a 0.3 home, and it does not open 0.3 Session logs.
