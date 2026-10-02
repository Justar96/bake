---
description: "The pi-ai-backed multi-provider adapter for users and maintainers routing the harness LLM service through pi-ai catalogs and hand-declared gateways."
kind: "package-reference"
---

# @deepseek-ai/dsh-llm-pi-ai

## Summary

`@deepseek-ai/dsh-llm-pi-ai` routes model requests to multiple pi-ai providers, OpenAI-compatible gateways, or self-hosted servers from one configuration. Installed pi-ai providers supply endpoint, protocol, and model-catalog defaults; custom routes can declare those values without code changes. Profiles and credentials are resolved for each request, so settings changes take effect on the next request without a restart. Supported providers can use stored OAuth or interactive-key sign-in with cross-process refresh locking. The package may start with no routes and activate when user settings add them.

## Table of Contents

- [Use this package](#use-this-package)
- [Understand the implementation](#understand-the-implementation)
- [Further Exploration](#further-exploration)
- [Model Experience](#model-experience)
- [Known Limitations and Deferred Work](#known-limitations-and-deferred-work)
- [Dev Note](#dev-note)

-----

<a id="use-this-package"></a>
## Use this package

Mount this plugin when a composition routes model requests through pi-ai's provider catalogs or through gateways that pi-ai's installed catalog does not describe. The `providers` dictionary is the whole configuration surface: each key is the provider route name a request selects with `GenerateOptions.provider`.

### When to choose it

This is the base composition's only LLM adapter: it serves DeepSeek through the shipped `deepseek-official` route as well as every other configured provider. Choose it when a route needs pi-ai's catalog defaults with a few fields corrected, or when a hand-declared gateway must be reached through its own endpoint and protocol. Registering a route another adapter already owns fails plugin loading.

### Configure provider routes

Each profile may set a `retryPolicy`; omission uses normal mode with five retries. `apiKeyEnv` is a credential reference resolved per request through the harness credential seam, so no secret enters the configuration file; a reference that resolves to nothing fails the request with `MISSING_CREDENTIAL`. Omitting it leaves the route configured-but-keyless, which for an installed catalog route defers to pi-ai's provider-native ambient discovery.

```yaml
- name: '@deepseek-ai/dsh-llm-pi-ai'
  config:
    providers:
      openai:
        apiKeyEnv: OPENAI_API_KEY
        baseURL: https://proxy.example.com:8443
        reasoning: high
        requestImagePixelBudget: 4194304 # total pixels; 2048 by 2048 default
        requestImageMaxBytes: 1048576    # raw bytes before base64 expansion
        maxRequestImageBytes: 20971520   # accumulated base64 payload
        retryPolicy:
          mode: normal
          maxRetries: 3
      anthropic:
        apiKeyEnv: ANTHROPIC_API_KEY
        models:
          - id: claude-sonnet-4-5
            contextWindow: 200000
      acme-gateway:
        displayName: Acme Gateway
        apiKeyEnv: ACME_GATEWAY_API_KEY
        api: openai-completions
        baseURL: https://gateway.acme.example/v1
        compat:
          thinkingFormat: deepseek
        models:
          - id: acme-think
            name: Acme Think
            contextWindow: 262144
            reasoningEfforts:
              off:
              high: high
```

| Field | Default | Meaning |
|---|---|---|
| `apiKeyEnv` | absent | Credential reference resolved per request; omission defers to pi-ai ambient discovery |
| `displayName` | provider name | Label shown by selector surfaces |
| `api` | catalog protocol | Wire protocol; only needed for routes the catalog does not supply. A `models` or `modelOverrides` entry's own `api` wins for that model |
| `baseURL` | catalog endpoint | Endpoint of every model on the route. A `models` or `modelOverrides` entry's own `baseURL` wins for that model |
| `models` | installed catalog | Replaces the route's catalog wholesale; each entry defaults from the installed model |
| `modelOverrides` | none | Reshapes individual installed-catalog models without replacing the rest |
| `compat` | catalog detection | Wire-compatibility switches for unrecognized endpoints |
| `defaultContextWindow` | `262,144` | Capacity fallback for undescribed models |
| `defaultMaxTokens` | `32,768` | Output-cap fallback for undescribed models |
| `requestImagePixelBudget` | `4,194,304` | Total-pixel budget for each deterministic request image |
| `requestImageMaxDimension` | `2000` on `anthropic-messages` models, none elsewhere | Long-edge cap for each request image, applied after the pixel budget; a set value applies to every model on the route |
| `requestImageMaxBytes` | `1 MiB` | Encoded-byte target for each request image before base64 expansion |
| `maxRequestImageBytes` | `20 MiB` | Aggregate base64 image-payload bound; a request whose retained images exceed it fails with `IMAGE_OFFLOAD_REQUIRED` |
| `retryPolicy` | normal, 5 retries | Provider-owned retry policy executed by `dsh-llm-retry` |
| `adaptiveThinkingType` | `adaptive` | Thinking spelling for `anthropic-messages` models with `compat.forceAdaptiveThinking`; `enabled` respells it for endpoints that accept only `enabled` and `disabled` |
| `messagesWire` | all off | Request-body rewrites for `anthropic-messages` models: `dropDeferredToolPlaceholder`, `stripCacheControl`, and `mergeAdjacentRoles`; refused on a route with no such model |

A `models` entry also accepts `description`, a one-line summary model selectors show, and the transcript-update declarations described in [Declare in-history updates](#declare-in-history-updates).

The generated [configuration catalog](../../../docs/config-catalog.md#deepseek-aidsh-llm-pi-ai) is the exhaustive source for every accepted field and its JSDoc.

### Use DeepSeek

The base composition ships one hand-declared route, `deepseek-official`, so DeepSeek works once `DEEPSEEK_API_KEY` is set. It speaks Anthropic-format Messages at `https://api.deepseek.com/anthropic`. It declares `deepseek-flash` (DeepSeek-V41-Flash, text and image input) and `deepseek-v4-pro` (text input). Both have a 1,000,000-token context window, a 256,000-token output cap, the efforts `off`, `low`, `high`, and `max`, and route-default reasoning `high`. DeepSeek accepts thinking only as `enabled` or `disabled` plus `output_config.effort`, so the route sets `adaptiveThinkingType: enabled`. `deepseek-flash` also declares in-history system-prompt and tool updates.

DeepSeek caches request prefixes automatically and does not need the scaffolding pi-ai adds for Anthropic's prompt cache, so the route turns on every `messagesWire` switch:

- `dropDeferredToolPlaceholder` removes the `__pi_deferred_placeholder__` tool pi-ai declares beside in-history tool updates. The placeholder stays when every other tool is deferred, because a request needs one tool that is not.
- `stripCacheControl` removes each `cache_control` breakpoint from the system prompt, tools, and message blocks, including blocks nested in a tool result.
- `mergeAdjacentRoles` joins adjacent messages of the same role into one message holding their content blocks in order, so a user prompt and the runtime context after it are sent as one user message.

The rewrites run on the request body after pi-ai builds it, together with the `adaptiveThinkingType` respelling. They change message boundaries and metadata only, not text.

Change one field of the shipped route through the `llm-pi-ai:` section of `$DSH_HOME/settings.yaml`, which merges per route and field, for example `baseURL` to reach a proxy. A Cordis `--patch` row for this plugin replaces its whole `config`, the shipped route included. pi-ai's own `deepseek` catalog route, which speaks Chat Completions, stays available beside it once given an `apiKeyEnv`.

<a id="declare-in-history-updates"></a>
### Declare in-history updates

A model entry may declare `systemPromptUpdate: in-history` and `toolUpdate: in-history` when its endpoint accepts system messages and tool changes in the middle of a conversation. Only an `anthropic-messages` model can declare them; any other protocol, and any other spelling, is refused where the profile is written. The model's `modelInfo` reports the declarations, so the harness can then send a later system prompt, or a tool addition or removal, at its place in history instead of rewriting the request's start.

With `systemPromptUpdate`, a later `system` message reaches the wire as a system message after the turn it follows, and a leading run of system messages collapses to its last one. With `toolUpdate`, the tools the conversation started with stay in the request's tool list, a tool added later is declared with deferred loading, and each change travels as a system message after the user turn that anchors it. pi-ai also adds one deferred placeholder tool, `__pi_deferred_placeholder__`, to such a request unless the route sets `messagesWire.dropDeferredToolPlaceholder`. A model without the declarations receives the current tool set and folds later system messages into user messages.

### Sign in to a provider

A provider pi-ai ships a login for can be signed into through the harness authorization seam: the flow offers OAuth or an interactive key prompt (a key is typed into pi-ai's own login prompt), and the resulting credential is stored in the harness credential store at `llm-pi-ai/<provider id>`. The stored sign-in authenticates its route beneath any `apiKeyEnv` override and refreshes itself under the store's cross-process lock; signing out deletes the stored record. A hand-declared route key outside the record grammar — a lowercase hyphenated identifier — cannot be signed into, because a record write for it refuses with `LlmError('UNSTORABLE_PROVIDER_ID')`; such a route authenticates through `apiKeyEnv` or ambient provider settings instead.

### Resolve the model catalog

A profile's `models` list replaces the route's installed catalog rather than extending it; each entry defaults its unset fields from the installed model of the same id, so narrowing a route to two models, correcting one capacity, or adding a model newer than the installed catalog are one-line edits. `modelOverrides` reshapes individual installed-catalog models without that cost — correct one model, keep the other thirty-seven — and is refused when set beside a `models` list, on a hand-declared route, or naming a model the catalog does not describe, because a silently unchanged model would be a typo someone hunts for later. An entry's own `api` moves that model alone onto another wire protocol, so a gateway that translates some upstreams poorly can serve each model over the protocol it relays cleanly under one route key and credential. The protocols join different paths onto an endpoint — the OpenAI ones append `/responses` or `/chat/completions` to a `/v1` base, Anthropic Messages appends `/v1/messages` to the server root — so a model moved to another protocol can also name its own `baseURL` for the same gateway.

A hand-declared route's entry has no installed model to default from, because its route key is not a catalog provider. An entry whose id the family's vendor publishes (`claude-` by `anthropic`, `gpt-` and the `o` series by `openai`, `gemini-` by `google`, `grok-` by `xai`) defaults its unset context window, output capability, and inputs from that vendor's entry. When the entry also speaks the vendor's protocol, it takes the vendor's effort levels and adaptive thinking too, since those are spelled for that protocol; no other compat switch crosses over. This is what lets a relay such as CLIProxyAPI, which lists `claude-sonnet-5-5` without its efforts, serve every effort Anthropic documents, in the terminal and headless alike, with nothing written to settings. A relay suffix or alias such as `:batch` or `-latest` matches nothing, and `reasoningEfforts: false` still declares a non-reasoning model.

### Run with reasoning and wire compatibility

`reasoningEfforts` declares a model's selectable thinking levels: each key is a level selectors offer, its value the spelling dispatch sends on the wire, so `max: ultra` renames a level for a gateway with its own vocabulary. Omitting the field keeps the installed catalog entry's capability; `false` declares a non-reasoning model. `compat` switches reshape the request for endpoints pi-ai cannot recognize — which role carries the system prompt, which field caps output, how a thinking level travels — configurable per route and per model. A model neither the entry nor the installed catalog sizes takes the route's `defaultContextWindow` and `defaultMaxTokens` fallbacks.

For self-hosted Chat Completions endpoints, `thinkingTokenBudgetField` selects the reasoning-budget parameter, and `vllmPriority` sets an integer scheduler priority when the server enables priority scheduling. Template arguments accept `$var: thinking.budget`. `openai-responses` gateways can set `supportsMaxOutputTokens: false` to omit `max_output_tokens`; Azure and Codex transports ignore this shared compatibility field. These controls are opt-in; catalog-owned Anthropic effort and fallback capabilities are not configurable switches.

### Change configuration at runtime

Profiles are re-read once per operation through the optional settings seam: the base and the user's `llm-pi-ai:` settings section merge per provider, so a user can add a route, override one field of a composition route, or point a route at another proxy, all effective on the next request with no restart. A section the adapter could not serve is refused where it is written — `settings.mutate` answers `settings-rejected` — and a stored section that later fails keeps the namespace's last good value. When the route set or a route's retry policy changes, the plugin re-registers atomically: a conflicting route leaves the previous routes serving.

### Discover models from endpoints

The plugin answers "which models can this provider serve?" for a route a configuration surface is editing or drafting. A route the installed catalog ships is answered from that catalog with no network call, preserving its `input` array as discovery `inputModalities`; only a route the catalog does not describe is interrogated over the wire. `openai-completions` and `openai-responses` use `GET {baseURL}/models` with bearer auth, while `anthropic-messages` uses native `GET /v1/models?limit=1000` semantics with `x-api-key` and `anthropic-version`; its listing URL accepts the API root with or without a trailing `/v1` because gateway documentation publishes both spellings, and only that listing URL normalizes the segment, so model requests receive the configured `baseURL` unchanged. A named configured route supplies its stored credential and profile `headers` inside the Host, so deployment headers configured through `settings.yaml` or Cordis config reach model discovery without becoming discovery-request fields; a key the caller supplies still wins over the stored credential. The parser accepts either the standard `data` array or an enriched `models` map, normalizing each candidate's id, display name, context window, and output-token cap; Anthropic's `max_input_tokens` and `max_tokens` feed the same capacity fields, a map key remains the request id even when its entry names a different canonical id, primitive-valued map properties are ignored, and a missing display name falls back to that request id. The reply is candidate metadata a surface may offer for adoption — nothing is stored, and `settings.yaml` remains the only thing that decides what a route serves.

### Failures and recovery

OpenAI Responses, Chat Completions, and Anthropic Messages HTTP streams are normalized for proxy heartbeats before pi-ai parses them: empty SSE events are ignored, including a named event whose `data:` line a heartbeat cut off. A gateway's heartbeat timer, such as CLIProxyAPI's `streaming.keepalive-seconds`, can land between an event's `event:` and `data:` lines, leaving the data as an unnamed event. On Anthropic Messages, an unnamed event, or one with the SSE default name `message`, whose data is a JSON object with an Anthropic stream event `type` (`message_start`, `content_block_delta`, `error`, and the rest) is delivered under that name, so its text is not lost and an `error` fails the turn with the provider's message. Named events keep their name, and unnamed data that is not such an object stays unnamed. Nonempty event data is never invented or rewritten and still goes through pi-ai validation; malformed JSON remains a failure. EOF without a Responses terminal payload produces `TRANSPORT`, eligible for the configured retry policy, rather than a successful empty response. Cancellation and generation idle timeouts remain active; heartbeats do not count as model progress. This normalization does not apply to other protocols or WebSocket transport.

A route pi-ai does not ship needs `api`, `baseURL`, and a non-empty `models` list; an unserviceable profile is refused where it is written, naming the route and model. Failures carry stable codes: a credential that cannot be used fails with `INVALID_CREDENTIAL` naming the route and reference, a route whose `apiKeyEnv` reference resolves to nothing fails with `MISSING_CREDENTIAL`, an unconfigured model fails with `UNKNOWN_MODEL`, and terminal provider failures distinguish `QUOTA` from transient `RATE_LIMIT`. A `RATE_LIMIT` or `SERVER` failure whose body carries a gateway credential-cooldown hint (`"reset_seconds": N`, sent by CLIProxyAPI and CliRelay when every credential for the model is cooling down) reports that wait as the provider retry delay: the retry policy waits exactly that long when it fits `retryPolicy.maxDelayMs`, and otherwise fails the turn at once instead of spending its retries inside the cooldown. Raise `maxDelayMs` on a gateway route to ride cooldowns out. `GenerateOptions.stop` is rejected with `UNSUPPORTED_OPTION` because pi-ai's common streaming UI cannot guarantee it across providers.

Settings writes strictly validate each new or changed provider after merging its composition and user layers. During namespace registration, stored catalog failures retain the namespace and provider rows, with the first available model diagnostic or route failure in `LlmConfigurableProvider.error`; unchanged failed providers do not block edits elsewhere. Serviceable models remain selectable, while unresolved models remain in the editable configuration and fail with `INVALID_CONFIG` before network I/O if requested directly. Repairing or deleting the offending configuration clears its diagnostic. Schema and self-contained profile errors still reject loading. Later external edits validate changed providers and retain the last accepted section on failure.

Changing `displayName`, `apiKeyEnv`, or `baseURL` without resolving the provider's model errors still rejects the save. For example, renaming an OpenRouter route whose model `111` needs an `api` cannot be saved on its own: repair or remove that model in the same editor draft, then save the complete provider configuration. Intermediate repairs remain in the draft until the whole provider validates; other providers can be saved independently.

-----

<a id="understand-the-implementation"></a>
## Understand the implementation

<details>
<summary>Implementation internals — click to expand</summary>

This section explains the design behind the adapter; the observable behavior is fully covered in [Use this package](#use-this-package).

### Design philosophy

The adapter is built on immutable snapshots and per-operation resolution. Each operation captures a whole snapshot — the profiles plus a `createModels()` collection holding the `Provider` each route built — before its first `await`, and a configuration change builds a new collection rather than mutating the one in use, so a request that started under one configuration never finishes under another. A route's own credential reference resolves through the harness seam and rides as the request's `apiKey` option, which pi-ai treats as the highest-priority auth override — that is what keeps the fail-loud reference semantics. Everything that override does not cover reaches pi-ai through the collection's own auth: the credential store holds the records a login wrote and a refresh rotates (addressed as `llm-pi-ai/<provider id>`), and the auth context answers the ambient questions a provider asks while resolving. Both are stable across snapshots, so a configuration change rebuilds the collection without forgetting who is signed in. Runtime imports use pi-ai's provider, API, and utility entry points; `src/models.ts` supplies the small model-helper subset this adapter needs without evaluating pi-ai's aggregate entry point.

### Source map

| File | Role |
|---|---|
| [`src/index.ts`](src/index.ts) | Plugin entry: profile resolution, settings wiring, directory and route registration |
| [`src/auth.ts`](src/auth.ts) | The credential store and ambient auth context over the harness credential plane |
| [`src/login.ts`](src/login.ts) | Authorization flows for the installed providers that ship a login |
| [`src/config.ts`](src/config.ts) | Profile schema, resolution, and serviceability checks |
| [`src/catalog.ts`](src/catalog.ts) | Installed-catalog integration and drift gates |
| [`src/models.ts`](src/models.ts) | Model collections, static providers, and reasoning levels over narrow pi-ai entry points |
| [`src/provider.ts`](src/provider.ts) | The supported-protocol table and provider construction |
| [`src/context.ts`](src/context.ts) | Harness-to-pi-ai context conversion, image handling, replay restore |
| [`src/payload.ts`](src/payload.ts) | `adaptiveThinkingType` and `messagesWire` rewrites of the Messages request body |
| [`src/stream.ts`](src/stream.ts) | pi-ai event conversion into harness `StreamChunk` values |
| [`src/replay.ts`](src/replay.ts) | Versioned `ReplayEnvelope` storage and validation |
| [`src/discovery.ts`](src/discovery.ts) | Endpoint interrogation for configuration surfaces |

### Registration and directory

The plugin declares every installed catalog provider it can authenticate in the configurable-provider directory, joined with every route the current profiles declare, so configuration surfaces can offer the full catalog before any route exists. Each entry carries `declared` — whether pi-ai ships nothing under that key — because only the adapter can distinguish a hand-declared route from a narrowed catalog route. Route registration is atomic: a candidate set that collides with another adapter leaves the previous routes serving. A bare mount with zero routes is the dormant posture: nothing registers until a settings section supplies profiles, and routes drop when it empties.

### Replay and vocabulary

Successful assistant responses store a versioned, lossless-JSON replay state beside the provider and model that produced them — response-level facts plus one per-block entry per streamed block. At request time, `LlmRuntime` passes replay state only when the same adapter instance owns both routes; the adapter validates it and restores native response ids, provider signatures, and optional `providerThinkingLevel` effort metadata, keeping absent effort metadata absent. Replay validates the requested model identity against the assistant source and separately restores an Anthropic response model when the provider resolved an alias or fallback. An unusable state degrades to provider-neutral content instead of failing the request. pi-ai tool-call arguments are parsed objects, so the adapter parses input and re-stringifies output to the harness raw-JSON convention; pi-ai in-stream error events map to terminal `finish` chunks.

</details>

-----

<a id="further-exploration"></a>
## Further Exploration

Read these pages when the package-level contract is not enough. They move from the service contract to the streaming protocol and the shared configuration.

- [dsh-llm service](../llm/README.md) — the provider-neutral service this adapter registers on.
- [LLM streaming subsystem](../../../docs/subsystems/llm-streaming.md) — the `StreamChunk` protocol and adapter contract.
- [llm-retry](../llm-retry/README.md) — the retry executor that applies each profile's `retryPolicy`.
- [Generated configuration catalog](../../../docs/config-catalog.md#deepseek-aidsh-llm-pi-ai) — every accepted config field and its source declaration.

-----

<a id="model-experience"></a>
## Model Experience

### Provider request through pi-ai

#### What the model sees

The selected catalog model receives one system prompt (`GenerateOptions.system`, otherwise the text of a leading `system` history message; a leading system message with empty text sends none), the remaining history, tools, and sampling fields supported by pi-ai's common streaming API. Each retained image is preceded by text naming its complete attachment id and actual request dimensions. Its request version is derived from the stored normalized attachment when the request is built: the route's `requestImagePixelBudget` first, then its `requestImageMaxDimension` long-edge cap. A model speaking `anthropic-messages` defaults to a 2000 px cap, Anthropic's per-side limit for a request carrying more than 20 images, so a long image history stays accepted. When the current execution filesystem maps the attachment provider's host object, the text also carries a read-only normalized-object path and warns that normalization or request projection may have resized or re-encoded the upload. Each occurrence selected by a logged image-offload decision keeps its own identity and currently resolved access in replacement text, and its normalized attachment is not read or transformed. When the retained occurrences' exact base64 payload still exceeds the route's `maxRequestImageBytes`, the call fails with `IMAGE_OFFLOAD_REQUIRED` so `dsh-compaction-image-offload` records the selected occurrences in an `image/offload` event and retries the step. Provider-native replay metadata is restored only when the adapter validates it for the historical content.

A model that declares `systemPromptUpdate: in-history` sees a later system prompt as a system message at its place in history; one that declares `toolUpdate: in-history` sees the tools it started with, later tools marked for deferred loading, pi-ai's `__pi_deferred_placeholder__` tool, and each tool change as a system message after its anchoring user turn. On `deepseek-official`, thinking is requested as `enabled` with the selected effort, and `off` sends `disabled`. Its `messagesWire` switches remove the placeholder tool and every `cache_control` marker, and join adjacent same-role messages, so the user prompt and the runtime context that follows it reach the model as one user message with both text blocks.

#### Token effect

Provider tokenization governs exact input. Retained images add the stable attachment and coordinate descriptor; the offload placeholder replaces an omitted image's visual tokens. A lower pixel budget or long-edge cap lowers an image's visual tokens. Claude's standard tier already downsizes above a 1568 px long edge, so the 2000 px default costs it nothing, while the high-resolution tier of Claude 4.7 and later, up to 2576 px, receives at most 2000 px. Replay metadata may let a native API reuse provider-side state. An in-history tool update adds the deferred tool definitions and a short change message instead of re-sending the whole tool list. `messagesWire.dropDeferredToolPlaceholder` saves the placeholder tool's definition, and `mergeAdjacentRoles` saves the per-message framing of each merged boundary.

#### KV Cache effect

Conversion preserves logical request order, while image handles and offload placeholders add model-visible text. Each image's request target depends only on its stored dimensions, the route's image settings, and the dispatching model's protocol, never on how many images the request carries, so a 21st image leaves the bytes of every earlier image, and the prefix cached through them, unchanged. Changing `requestImagePixelBudget`, `requestImageMaxDimension`, or `requestImageMaxBytes` re-projects every retained image, so reuse ends at the first one. The upgrade that introduced the 2000 px Anthropic Messages default has the same one-time effect on a session whose history holds a larger image; later requests are stable again. A changed execution-world path rewrites a historical handle and can prevent reuse from that image even when attachment identity and request bytes stay stable. Changing adapter instance, provider, model, or another upstream token has the same suffix effect. Which prefix a provider reuses is decided by the wire protocol: OpenAI-family endpoints cache automatically and receive the session id as `prompt_cache_key`, while Anthropic Messages caches only at the `cache_control` breakpoints pi-ai marks on the system prompt, the last tool, and the last user message. An Anthropic-compatible endpoint that caches prefixes on its own, as DeepSeek does, needs none of them, and `messagesWire.stripCacheControl` removes them. The `messagesWire` rewrites depend only on the request body pi-ai builds, so they are the same on every step and do not break the prefix between steps; changing a switch changes the body from its first rewritten position, which costs one cache miss. A Claude model a gateway serves over a translated OpenAI protocol therefore gets no prompt caching. Provider caches are also scoped to one upstream credential, so a gateway that round-robins several credentials re-prefills the whole prefix whenever consecutive steps land on different ones. Every request that belongs to a session therefore carries its id as `x-deepseek-harness-session-id`, the header CliRelay's `session-sticky` routing binds on; a profile header cannot override or forge it. For a gateway that keys on another header instead, setting `compat.sendSessionAffinityHeaders: true` on an Anthropic Messages or Chat Completions route sends the session id as `x-session-affinity`, which a gateway with sticky routing enabled (CLIProxyAPI's `routing.session-affinity`) uses to keep a session on one credential; Responses routes need no switch because `prompt_cache_key` already carries the id. An offload decision turns an earlier image into placeholder text, so reuse ends at that message; the omission never reverts, so the prefix stays stable afterwards. On a model with in-history declarations, a changed system prompt or tool set appends after the cached prefix instead of rewriting the request's start, so the earlier prefix stays reusable.

### Provider response

#### What the model sees

pi-ai events become harness reasoning, text, tool-call, usage, and finish chunks. The adapter passes parsed tool arguments to the harness as raw JSON strings.

#### Token effect

Generated content affects later inputs only after the loop records it. pi-ai folds reasoning tokens into output usage when the provider does not report them separately, and preserves its exact `totalTokens` value unchanged. A stream whose endpoint sent no accounting, which pi-ai leaves at all zeros, yields no usage chunk, so the context meter reads unknown rather than empty.

#### KV Cache effect

Recorded response content appends to the next request and does not invalidate its earlier reusable prefix. Unrecorded transport metadata and usage accounting do not affect cache identity.

## Known Limitations and Deferred Work

<a id="known-limitations-and-deferred-work"></a>


These limits define where the adapter stops and future work begins. They are current package constraints, not a general pi-ai comparison or a task backlog.

- **`maxRequestImageBytes` counts base64 image payload only** — text, tools, descriptors, and JSON structure ride outside the bound, so it must sit below the gateway's request-body cap with headroom.
- **The default long-edge cap follows the wire protocol, not the upstream model** — Claude served over `openai-completions` or `openai-responses`, as OpenRouter's catalog and translating gateways do, takes no default cap, so a request with more than 20 images and a side above 2000 px fails with Anthropic's many-image error; set `requestImageMaxDimension: 2000` on that route. The value applies to every model on the route. Raising the cap above 2000 px on an Anthropic Messages route, for Claude's 2576 px high-resolution tier, fails the same way once history holds more than 20 images.
- **A sign-in lives only in the process that started it** — an authorization attempt is not durable, so reloading the page mid-login abandons it and the human starts over. Signing out is `deleteRecord` on the stored record, which forgets it locally without telling the issuer.
- **Provider-native discovery answers through this plugin's ambient context** — a route naming no credential defers to the catalog provider's own resolution, which asks for environment values (`AZURE_OPENAI_API_KEY`, `AWS_PROFILE`, and each provider's own set) and for local credential files. Both questions are answered here: the credential seam is consulted before the process environment, and file existence is checked against the host process's filesystem with `~` expanded. What it cannot do is *read* a credential file's contents — a provider that parses `~/.aws/credentials` itself does so directly, outside the seam.
- **Settings can add or override routes, not remove composition routes** — the user layer merges over the composition base, so deleting a `cordis.yml`-provided provider is a composition change.
- **The layered merge has no delete for dict keys** — a `reasoningEfforts` level, `modelOverrides` entry, or `compat` field the base declares can be overridden but not removed by the user layer.
- **`headers` can carry a credential the redactor never sees** — profile resolution rejects names and values Fetch cannot represent, but the dict remains plain strings; store credentials as `apiKeyEnv` references.
- **A route's catalog never refreshes itself** — the catalog is whatever `settings.yaml` says; nothing here queries a provider for the models it serves.
- **Anthropic discovery reads at most 1,000 models** — the request uses the API's maximum page size but does not traverse `has_more`; entries beyond the first page must be added by hand.
- **A modality declaration is not verified** — a model declaring `image` its gateway does not serve is refused by the provider after prompt admission. The durable image remains in history and the same misdeclared model can fail again; switching to a text-only model remains possible because the shared LLM runtime projects image references into stable text for that request.
- **An unauthenticated route depends on its protocol** — a route naming no credential resolves as configured-but-keyless, but pi-ai's OpenAI-compatible implementation still requires an API key or an `Authorization` header, so a keyless local server needs a placeholder credential referenced by `apiKeyEnv` or an `Authorization` entry in `headers`.
- **In-history updates are declared, not detected** — nothing probes an endpoint for mid-conversation system messages or tool changes, and only `anthropic-messages` models may declare them. A tool update must be anchored on a user turn present in the request, and an in-history system prompt must not be empty; otherwise the request fails with `INVALID_REQUEST`.
- **The shipped DeepSeek route has no DeepSeek-specific extras** — it sends no account usage query, Files-API upload, request extensions, or DeepSeek image-token pricing; images travel inline, and the context meter estimates image tokens. A model id the route does not declare fails with `UNKNOWN_MODEL`.
- **`GenerateOptions.stop` is unsupported** — pi-ai's common stream options cannot guarantee stop-sequence behavior across providers.
- **Without `systemPromptUpdate`, only a leading in-history `system` message becomes pi-ai's `systemPrompt`** — pi-ai has one system slot, so a later `system` message, or a leading one when `GenerateOptions.system` is also set, folds into a `user` message at its position; provider-specific placement of the prompt follows pi-ai rather than a harness-owned wire override. Images in system or assistant history, including the leading system message, fail with `UNSUPPORTED_CONTENT` on both conversion paths.
- **Provider HTTP status is unavailable** — pi-ai error events do not expose a stable HTTP status across providers.
- **Retry policy is provider-owned, not an SDK retry** — pi-ai SDK retries stay disabled so durable agent steps and `llm/retry` events own every visible attempt, and direct `ctx.llm.stream()` calls remain single-attempt.

<a id="dev-note"></a>
### Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

This Dev Note is non-authoritative working context: undecided directions and notes for maintainers. Shipped behavior and accepted rationale live in the sections above, the package code, and the linked Agent Notes.

- The offered protocol set is deliberately narrower than pi-ai's full API set: Bedrock, Vertex, Azure, and Codex authenticate through flows a profile cannot completely describe with a key, an endpoint, and headers; catalog routes still reach them through their own provider, and only an explicit override is refused. Codex is sign-in-able through the authorization flow's OAuth grant.
- The `compat` switch set is pinned to pi-ai's compat types by drift gates; an upstream upgrade that adds a field, gives a further protocol a compat type, or widens a value union fails the build until someone classifies it.

</details>

**Runtime invariant:** No companion is published. This package exposes no independent event sequence or mutable data relation beyond contracts enforced at its owning seam.
