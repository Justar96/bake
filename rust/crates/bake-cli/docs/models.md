# Models and providers

bake-rs has no built-in model catalog: every model comes from `models.json` in the Bake home or from the CLIProxyAPI account. API keys come from `models.json`, `auth.json`, or that account. bake-rs never writes `models.json`; only the interactive commands write `auth.json` and `settings.json` (`/login`, `/logout`, `/refresh-models`, and saving a default).

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

A request's key is the first of: `--api-key`, the provider's key in `auth.json`, its `apiKey` in `models.json`, then the imported CLIProxyAPI key. The CLIProxyAPI account's key is the exception: it is used only with its own route (see below). A credential may carry `env`, values that its key and headers read before the process environment.

## Keys and header values

`apiKey`, `key`, and header values may be:

- `!command`: runs the command in a shell (`/bin/sh` on Unix, `%ComSpec%` or `cmd.exe` on Windows) and uses its trimmed output. It has 10 seconds and 1 MiB of output.
- `$NAME` or `${NAME}`: the environment variable. A value naming any unset variable resolves to nothing.
- `$$` for a literal `$`, and `$!` for a literal `!`.
- Anything else, used as written.

## The CLIProxyAPI account

In an interactive session, `/login` signs in to a CLIProxyAPI proxy. It asks for the proxy's address (its root, `/v1`, or `/backend-api` URL; empty means `http://127.0.0.1:8317`) and API key, reads the proxy's model list with that key, and saves the account as the `cliproxyapi` provider with the models listed. Esc cancels until the list has been read; nothing is saved then.

- The key goes to `auth.json`, as the `cliproxyapi` record. The route (address and models) goes to the `cliproxyapi` member of `settings.json`. Both carry the same `account` value, a random tag each sign-in draws.
- bake-rs uses the route only with the key whose tag matches. When they do not match, for example after another program replaced the key or after a crash between the two writes, the account is off: a startup note says so, there is no `cliproxyapi` provider, and `/login` recovers. The account's key is never sent to another route.
- `/refresh-models` reads the model list again and saves it, keeping the current model and the default model. Each interactive session also refreshes the list once at start, quietly, unless a saved list may not survive a power loss, which it reports. `--offline`, `BAKE_OFFLINE=1`, or `DSH_OFFLINE=1` leaves that refresh out: it is the only network operation a session makes before its first prompt, so with it an offline session starts entirely from files. `/login` and `/refresh-models`, which you ask for, still reach the proxy.
- `/logout` sets the route to `null` and removes the key. A signed-out home no longer imports the route Bake 0.3 saved.
- `--api-key` still comes first for requests and for `/refresh-models`.

## Checking and printing credentials

```sh
bake-rs auth check --provider cliproxyapi          # ready, not_ready, or invalid
bake-rs auth check --provider cliproxyapi --json   # the same as one JSON line
bake-rs auth print-api-key --provider cliproxyapi  # the key, on standard output
```

- `bake-rs auth check` prints one word: `ready` when the provider exists and a key is configured for it, `not_ready` when the provider is missing or has no key, and `invalid` when `models.json` could not be composed. It exits 0, 1, and 2 to match. `--json` prints `{"status":…,"provider":…}` with `reason` or `authType` instead, and `--credentials` prints the key itself, or includes it as `credentials` in the JSON.
- `bake-rs auth print-api-key` prints a provider's API key on standard output. With `--provider` it names the provider; with `--model` alone it searches the providers that have a key in `auth.json` for one whose model that names exactly. More than one match, or none, is an error naming `--provider`.
- `bake-rs auth print-bearer-token` has no token to print: bake-rs has no OAuth provider, so it always fails with the reason. `--min-expiry <30m|1h|…>` is checked and then unused.
- `--no-refresh` is accepted and changes nothing: nothing to refresh, and these commands never write. All three read `models.json`, `auth.json`, and `settings.json` only, reach no network, and accept no flag but `--provider` and `--model`; `--api-key` is refused. `bake-rs auth` prints their help.
- A key is printed only by `print-api-key` and by `check --credentials`, whose purpose is to print it; no error message or status line carries one.

## The route Bake 0.3 saved

While `settings.json` has no `cliproxyapi` member, a CLIProxyAPI route that Bake 0.3's login saved becomes the `cliproxyapi` provider with the models saved with it. Its key is `CLIPROXYAPI_API_KEY` when that environment variable is set and not empty, else the key in `.credentials.yaml`. Signing in replaces it; the 0.3 files are never changed. `bake-rs docs home` lists what else bake-rs imports.
