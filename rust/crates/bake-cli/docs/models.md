# Models and providers

bake-rs has no built-in model catalog: every model comes from `models.json` in the Bake home or from the CLIProxyAPI route Bake 0.3 saved. API keys come from `models.json`, `auth.json`, or that route. bake-rs reads these files and never writes them.

## Choosing a model

- `--model <pattern>` picks a model by id, by `provider/id`, or by part of an id or name; when several match, an alias is preferred over a dated id. A `:<level>` suffix sets the thinking level, as in `--model cliproxyapi/gpt-5.5:high`.
- `--provider <name>` limits `--model` to one provider. It needs `--model`.
- `--models <patterns>` scopes the available models to a comma-separated list of model patterns or globs (such as `cliproxyapi/*:high,*sonnet*`). For a new session without `--model`, bake-rs picks the saved default model if it is in the scope, else the first model in the scope.
- `--api-key <key>` sets a non-persistent runtime API key for the provider of the model selected by `--model`, `--provider`/`--model`, or `--models`.
- `--thinking <level>` sets the thinking level: `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, or `max`. It wins over a suffix. A level the model does not support is clamped to one it does.
- `--list-models [search]` lists the available models (provider, model id, context window, max output tokens, thinking support, and image support), optionally filtered by a fuzzy search pattern, and exits.

Without `--model`, a resumed session keeps the model it last used while that model's provider has a key. Otherwise bake-rs uses `defaultProvider` and `defaultModel` from `settings.json` (else the default model imported from Bake 0.3) when that provider has a key, else another model whose provider has one. A run with no usable model fails before sending anything. The ids `--model` accepts are the `id` members in `models.json` and the ids the imported route saved; `--list-models` prints them.

## models.json

`models.json` maps provider ids to providers. `//` line comments and trailing commas are allowed. A file that fails to parse or validate adds no providers and prints a warning that names each problem.

```json
{
  "providers": {
    "my-gateway": {
      "baseUrl": "https://gateway.example.com/v1",
      "api": "openai-completions",
      "apiKey": "$MY_GATEWAY_KEY",
      "models": [
        { "id": "my-model", "name": "My model", "reasoning": true, "input": ["text", "image"], "contextWindow": 200000, "maxTokens": 32000 }
      ]
    }
  }
}
```

- A provider holds `baseUrl`, `api`, `apiKey`, `headers`, `authHeader` (when `true`, sends `Authorization: Bearer <key>`), `compat`, `models`, and `modelOverrides` (changes to models by id).
- `api` is `openai-completions`, `openai-responses`, or `anthropic-messages`, set on the provider or on a model. A provider that defines models needs `baseUrl`.
- A model needs `id`. It may set `name`, `api`, `baseUrl`, `reasoning`, `input` (`text` and `image`), `contextWindow`, `maxTokens`, `cost` (`input`, `output`, `cacheRead`, `cacheWrite`), `headers`, `compat`, and `thinkingLevelMap`.
- A provider with the id `cliproxyapi` changes the imported CLIProxyAPI route: its members, models, and key win over the route's.

## auth.json

`auth.json` maps provider ids to stored keys:

```json
{ "my-gateway": { "type": "api_key", "key": "!pass show gateway" } }
```

A request's key is the first of: the provider's key in `auth.json`, its `apiKey` in `models.json`, then the imported CLIProxyAPI key. A credential may carry `env`, values that its key and headers read before the process environment.

## Keys and header values

`apiKey`, `key`, and header values may be:

- `!command`: runs the command in a shell (`/bin/sh` on Unix, `%ComSpec%` or `cmd.exe` on Windows) and uses its trimmed output. It has 10 seconds and 1 MiB of output.
- `$NAME` or `${NAME}`: the environment variable. A value naming any unset variable resolves to nothing.
- `$$` for a literal `$`, and `$!` for a literal `!`.
- Anything else, used as written.

## The CLIProxyAPI route

When the Bake home holds a CLIProxyAPI route that Bake 0.3's login saved, bake-rs adds it as the `cliproxyapi` provider with the models saved with it. Its key is `CLIPROXYAPI_API_KEY` when that environment variable is set and not empty, else the key in `.credentials.yaml`. bake-rs cannot log in or refresh the saved model list; `bake-rs docs home` lists what else it imports.
