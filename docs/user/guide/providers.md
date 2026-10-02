# Configure models

This guide assumes you started Bake through the [root README](../../../README.md#get-started). Sign in with `/login`, choose a model with `/model`, and configure routes in `$DSH_HOME/settings.yaml`. Settings changes take effect on the next request without a restart.

## Configure DeepSeek

DeepSeek is the built-in `deepseek-official` route of [`dsh-llm-pi-ai`](../../../packages/llm/llm-pi-ai/README.md), which reaches DeepSeek's Anthropic Messages endpoint at `https://api.deepseek.com/anthropic` with the key in `DEEPSEEK_API_KEY`. Set that variable, or run `/login deepseek` in the terminal to store the key in `$DSH_HOME/.credentials.yaml`, where settings retain only its credential reference.

The route serves two models, each with a 1M-token context window and the reasoning efforts `off`, `low`, `high`, and `max`, starting at `high`:

| Model | Name | Input |
| --- | --- | --- |
| `deepseek-flash` | DeepSeek-V41-Flash | text and images |
| `deepseek-v4-pro` | DeepSeek-V4-Pro | text |

`deepseek-v4-pro` is stronger at agentic coding, knowledge, and difficult reasoning, and costs more.

A reasoning level other than `off` turns thinking on and sends the level as the request's effort; `off` turns thinking off.

To change the endpoint, the starting effort, or any other field, add a `deepseek-official` route under `llm-pi-ai` in `$DSH_HOME/settings.yaml`. It merges into the built-in route field by field, so it names only what changes:

```yaml
llm-pi-ai:
  providers:
    deepseek-official:
      baseURL: https://deepseek-gateway.example/anthropic
      reasoning: max
```

A list replaces the built-in one whole, so a `models` list must restate every model the route keeps.

## Add a built-in provider

Run `/login` and pick a provider: besides DeepSeek and CLIProxyAPI, it offers OpenAI, Anthropic, GitHub Copilot, OpenRouter, Kimi, and xAI. Signing in adds the provider's route, so its models reach `/model` at once, and a session that had no model starts on that provider's model. The installed catalog supplies the endpoint, protocol, and model list.

## Add a custom provider

Add a route under `llm-pi-ai` in `$DSH_HOME/settings.yaml` for a company gateway, a self-hosted server, or a provider absent from the installed catalog. Give it a lowercase provider id, a base URL, an API protocol, a credential reference, and at least one model:

```yaml
llm-pi-ai:
  providers:
    my-gateway:
      displayName: My gateway
      apiKeyEnv: GATEWAY_API_KEY
      api: openai-completions
      baseURL: https://gateway.example/v1
      models:
        - id: my-model
```

`api` must be the protocol your gateway speaks: `openai-completions` for OpenAI Chat Completions, `openai-responses` for the OpenAI Responses API, or `anthropic-messages` for the Anthropic Messages API. A provider speaks one protocol, so a gateway that serves two needs two providers.

The provider id is permanent because requests, saved sessions, model defaults, and credential references use it. To rename a provider, add a new route and delete the old one. The display name, base URL, protocol, credential, and models remain editable.

## Select a model

Configured providers appear in the `/model` picker. Selecting a model also makes it the default for new sessions. A session that has already sent a request retains the model recorded in its own log.

If no model is selected, the composer refuses to send until you sign in with `/login` or choose a model with `/model`.

## Advanced configuration

The generated [plugin configuration catalog](../../config-catalog.md) lists every supported field and default for every plugin; [`dsh-llm-pi-ai`](../../config-catalog.md#deepseek-aidsh-llm-pi-ai) is the provider section this page configures. The [`dsh-llm-pi-ai`](../../../packages/llm/llm-pi-ai/README.md) reference owns direct `settings.yaml` configuration, catalog resolution, reasoning controls, credentials, and adapter errors.

::: tip Additional settings
Besides the fields above, `$DSH_HOME/settings.yaml` configures each model's context window, max output tokens, and input types, along with reasoning effort levels, request-compatibility switches, headers, timeouts, and retry policy. The adapter re-reads it on the next request, so nothing needs a restart. The subsections below cover the fields most gateways need.
:::

### Image input

A model's `input` lists the input types it accepts. For example, this custom pi-ai provider declares one text-only model and one vision model:

```yaml
llm-pi-ai:
  providers:
    my-gateway:
      apiKeyEnv: GATEWAY_API_KEY
      api: openai-completions
      baseURL: https://gateway.example/v1
      models:
        - id: legacy-chat
        - id: vision-preview
          input: [text, image]
```

Pi-ai's `input` accepts `text` and `image` and applies to that model alone. An explicit nonempty selection takes priority. An omitted or empty `input` inherits the installed catalog's input types, then the route's `defaultInput`, which defaults to `[text]`. To restore inheritance, remove the model's `input` field.

If every model you entered by hand takes images, set the fallback once on the route instead of on each of them:

```yaml
llm-pi-ai:
  providers:
    vision-gateway:
      apiKeyEnv: GATEWAY_API_KEY
      api: openai-completions
      baseURL: https://vision.example/v1
      defaultInput: [text, image]
      models:
        - id: first-model
        - id: second-model
```

`defaultInput` is a fallback, not an override, and defaults to `[text]`: on a built-in provider it answers only for models its catalog does not describe, so it never removes images from a catalog model that has them. Narrow one of those with that model's own `input`. When a built-in provider has no explicit `models` list, write it under `modelOverrides`, keyed by model id:

```yaml
llm-pi-ai:
  providers:
    anthropic:
      modelOverrides:
        claude-sonnet-4-5:
          input: [text]
```

In pi-ai configuration, every list must name at least one modality except a model's own `input`, where an empty list means the same as omitting it. An unknown modality is refused wherever it is written.

Both fields state a claim about your endpoint rather than checking it. A model that declares images its endpoint does not serve is not caught here; the provider rejects the request instead.

### Reasoning effort

The `/model` picker offers reasoning levels for a model that declares them. A built-in provider's models inherit their levels from the installed catalog. A model you enter by hand declares none, so the picker offers no levels for it and the endpoint's own default decides whether the model thinks. Declare the levels with `reasoningEfforts` in `$DSH_HOME/settings.yaml`:

```yaml
llm-pi-ai:
  providers:
    my-gateway:
      apiKeyEnv: GATEWAY_API_KEY
      api: openai-completions
      baseURL: https://gateway.example/v1
      reasoning: high
      models:
        - id: my-reasoner
          reasoningEfforts:
            off:
            high: high
            max: max
```

Each key is a level the picker offers, and its value is the spelling sent on the wire as `reasoning_effort`, so `max: xhigh` renames a level for a gateway with its own vocabulary. Only `off` may stay empty, because for most endpoints not thinking is the parameter's absence. The route's `reasoning` is the level used while a session has picked none; choosing an effort in the picker saves it, with the model, as the default for new sessions.

An `off` left empty sends nothing, which only stops a model that thinks on request; an `off` given a value sends that value as `reasoning_effort` instead. A model that thinks unless told not to — DeepSeek V4 behind an OpenAI-compatible gateway, for example — needs `compat.thinkingFormat: deepseek`, which makes `off` send `thinking: {type: disabled}` and every other level send `thinking: {type: enabled}` beside the effort:

```yaml
      models:
        - id: deepseek-v4-pro
          compat:
            thinkingFormat: deepseek
          reasoningEfforts:
            off:
            high: high
            max: max
```

A built-in provider's model whose gateway does not reason loses its levels with `reasoningEfforts: false` under `modelOverrides`; selecting an effort for it is then refused as `UNSUPPORTED_REASONING_EFFORT`. DeepSeek's own route needs none of this: its models already offer `off`, `low`, `high`, and `max`, and the route's `reasoning` sets the default the picker starts from:

```yaml
llm-pi-ai:
  providers:
    deepseek-official:
      reasoning: max
```

### Request compatibility

A gateway can hold a working key at a reachable address and still refuse every request. pi-ai decides the shape of a request — which role carries the system prompt, which field caps the output, how a thinking level travels — from the endpoint's URL, and an address it does not recognize is addressed as though it were OpenAI itself. Most OpenAI-compatible gateways refuse at least one thing OpenAI accepts.

Two account for most of it. A model that declares reasoning has its system prompt sent as `role: "developer"`, which many gateways reject outright, and the output cap is sent as `max_completion_tokens`, which a server that only knows `max_tokens` refuses. Correct them on the route in `$DSH_HOME/settings.yaml`:

```yaml
llm-pi-ai:
  providers:
    my-gateway:
      apiKeyEnv: GATEWAY_API_KEY
      api: openai-completions
      baseURL: https://gateway.example/v1
      compat:
        supportsDeveloperRole: false
        maxTokensField: max_tokens
      models:
        - id: my-model
```

A route's `compat` is the default for its models, and a model's own wins field by field, so one model can be corrected without restating the route:

```yaml
      models:
        - id: my-model
        - id: my-reasoner
          compat:
            thinkingFormat: deepseek
```

What neither sets keeps the installed catalog's value for that model, and what the catalog does not describe falls to pi-ai's detection. Give every switch you name a value: a key left empty (`supportsDeveloperRole:`) is refused rather than ignored, because an empty value would erase what the catalog knows while saying nothing in its place. A name no protocol accepts is refused too, and the message lists the ones that are available.

Each switch belongs to the protocols that declare it, so a switch valid on one `api` may be refused on another — the message names what that protocol does offer. Like `input` above, a switch states a claim about your endpoint rather than checking it: setting one your gateway does not actually need simply sends a different request.

Every switch, its accepted values, and the protocols that take it are listed under `PiAiCompatProfile` in the [generated `dsh-llm-pi-ai` configuration reference](../../config-catalog.md#deepseek-aidsh-llm-pi-ai) — which is derived from the source, so it cannot fall behind what the adapter accepts.

## Troubleshooting

- **`MISSING_CREDENTIAL`** — Store the provider key with `/login` or supply the referenced environment variable.
- **`UNKNOWN_MODEL`** — Select a configured model or add the missing model to the custom provider.
- **The gateway refuses every request although the key and URL are right** — Its request shape differs from OpenAI's. Start with `compat.supportsDeveloperRole: false` and `compat.maxTokensField: max_tokens` on the route.
- **Only reasoning models fail** — pi-ai sends their system prompt as the `developer` role, which the gateway rejects. Set `compat.supportsDeveloperRole: false`.
- **`/model` offers no reasoning levels for a model you entered by hand** — It declares no levels. Add `reasoningEfforts` to the model in `settings.yaml`.
- **`off` does not stop a DeepSeek model from thinking** — An empty `off` sends no reasoning field at all, and an endpoint that thinks by default keeps thinking. Set `compat.thinkingFormat: deepseek` on the model or the route.
- **A compat switch is refused as having no value** — A key written with nothing after the colon. Give it a value, or remove the key to keep the installed catalog's.
- **An image is refused before sending** — The model declares no image modality. Give a custom provider's model `input: [text, image]`; on DeepSeek's own route, select `deepseek-flash`, and if the route points at another endpoint, confirm that it serves that model with image input.
- **The provider rejects a request carrying an image** — The model declares images its endpoint does not actually serve. Remove `image` from whichever list granted it — the model's `input`, or the route's `defaultInput` — then start a new session: the attached image stays in the session log, so the same request repeats until the session moves off it.
